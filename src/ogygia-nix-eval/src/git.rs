//! Git repositories fetched over HTTP into memory.

use std::cell::RefCell;
use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use gix::ObjectId;
use gix::odb::pack;
use gix_transport::client::blocking_io::http;

use crate::fetch::Fetcher;
use crate::fetch::Method;
use crate::fetch::Request;
use crate::fetch::Response;

/// The objects of a pack, held in memory.
pub struct PackObjects {
    pack: pack::data::File<Arc<[u8]>>,
    index: pack::index::File<Vec<u8>>,
    inflate: RefCell<gix::zlib::Inflate>,
}

impl PackObjects {
    fn new(data: Vec<u8>) -> Result<PackObjects> {
        let data: Arc<[u8]> = data.into();
        let hash = gix::hash::Kind::Sha1;
        let mut entries = pack::data::input::BytesToEntriesIter::new_from_header(
            &*data,
            pack::data::input::Mode::Verify,
            pack::data::input::EntryDataMode::Crc32,
            hash,
        )?;
        let version = entries.version();
        let mut index = Vec::new();
        pack::index::write_data_iter_to_stream(
            pack::index::Version::default(),
            || Ok((resolve_entry, Arc::clone(&data))),
            &mut entries,
            Some(1),
            &mut gix::progress::Discard,
            &mut index,
            &AtomicBool::new(false),
            hash,
            None,
            version,
        )?;
        Ok(PackObjects {
            pack: pack::data::File::from_data(data, Default::default(), hash)?,
            index: pack::index::File::from_data(index, Default::default(), hash)?,
            inflate: RefCell::default(),
        })
    }
}

/// The bytes of the pack entry at `range` in `data`.
fn resolve_entry(range: pack::data::EntryRange, data: &Arc<[u8]>) -> Option<&[u8]> {
    data.get(range.start as usize..range.end as usize)
}

impl gix::objs::Find for PackObjects {
    fn try_find<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix::Result<Option<gix::objs::Data<'a>>> {
        let Some(idx) = self.index.lookup(id) else {
            return Ok(None);
        };
        let entry = self.pack.entry(self.index.pack_offset_at_index(idx))?;
        let base = |id: &gix::oid, _: &mut Vec<u8>| {
            self.index
                .lookup(id)
                .map(|idx| {
                    self.pack
                        .entry(self.index.pack_offset_at_index(idx))
                        .map(pack::data::decode::entry::ResolvedBase::InPack)
                })
                .transpose()
        };
        let outcome = self.pack.decode_entry(
            entry,
            buffer,
            &mut self.inflate.borrow_mut(),
            &base,
            &mut pack::cache::Never,
        )?;
        Ok(Some(gix::objs::Data {
            kind: outcome.kind,
            data: buffer.as_slice(),
            object_hash: self.pack.object_hash(),
        }))
    }
}

/// Fetch commit `rev` of the repository at the HTTP or HTTPS URL `url`
/// through `fetcher`, with its history unless `shallow`.
pub fn fetch_commit(
    fetcher: &dyn Fetcher,
    url: &str,
    rev: ObjectId,
    shallow: bool,
) -> Result<PackObjects> {
    use gix::protocol::fetch::Arguments;
    let parsed = gix::url::parse(gix::bstr::BStr::new(url))?;
    let mut transport = http::connect_http(
        Http { fetcher },
        parsed,
        gix::protocol::transport::Protocol::V2,
        false,
    );
    let handshake = gix::protocol::handshake(
        &mut transport,
        gix::protocol::transport::Service::UploadPack,
        |_| Ok(None),
        Vec::new(),
        &mut gix::progress::Discard,
    )?;
    let version = handshake.server_protocol_version;
    let features = gix::protocol::Command::Fetch.default_features(version, &handshake.capabilities);
    let mut args = Arguments::new(version, features, false);
    args.want(rev);
    if shallow {
        if !args.can_use_deepen() {
            bail!("'{url}' cannot send a single commit");
        }
        args.deepen(1);
    }
    let mut reader = args.send(&mut transport, true)?;
    let response =
        gix::protocol::fetch::Response::from_line_reader(version, &mut reader, true, false)?;
    if !response.has_pack() {
        bail!("'{url}' sent no objects for commit {rev}");
    }
    // Without a handler, progress messages are read as pack data.
    let discard: gix_transport::client::blocking_io::HandleProgress<'_> =
        Box::new(|_, _| std::ops::ControlFlow::Continue(()));
    reader.set_progress_handler(Some(discard));
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;
    PackObjects::new(data).with_context(|| format!("indexing the objects '{url}' sent"))
}

/// gix's HTTP client, making its requests through a [`Fetcher`].
struct Http<'f> {
    fetcher: &'f dyn Fetcher,
}

impl<'f> Http<'f> {
    fn exchange(
        &self,
        method: Method,
        url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Rc<RefCell<Exchange<'f>>> {
        let headers = headers
            .into_iter()
            .filter_map(|h| {
                let (name, value) = h.as_ref().split_once(':')?;
                Some((name.trim().to_owned(), value.trim().to_owned()))
            })
            .collect();
        Rc::new(RefCell::new(Exchange {
            fetcher: self.fetcher,
            method,
            url: url.to_owned(),
            headers,
            body: Vec::new(),
            response: None,
        }))
    }
}

impl<'f> http::Http for Http<'f> {
    type Headers = Received<'f>;
    type ResponseBody = Received<'f>;
    type PostBody = Sent<'f>;

    fn get(
        &mut self,
        url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> gix::Result<http::GetResponse<Received<'f>, Received<'f>>> {
        let exchange = self.exchange(Method::Get, url, headers);
        Ok(http::GetResponse {
            headers: Received::new(&exchange, true),
            body: Received::new(&exchange, false),
        })
    }

    fn post(
        &mut self,
        url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        _body: http::PostBodyDataKind,
    ) -> gix::Result<http::PostResponse<Received<'f>, Received<'f>, Sent<'f>>> {
        let exchange = self.exchange(Method::Post, url, headers);
        Ok(http::PostResponse {
            post_body: Sent(Rc::clone(&exchange)),
            headers: Received::new(&exchange, true),
            body: Received::new(&exchange, false),
        })
    }

    fn configure(&mut self, _config: &dyn std::any::Any) -> gix::Result {
        Ok(())
    }
}

/// A request, made when its response is first read.
struct Exchange<'f> {
    fetcher: &'f dyn Fetcher,
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    response: Option<Response>,
}

impl Exchange<'_> {
    fn response(&mut self) -> std::io::Result<&mut Response> {
        if self.response.is_none() {
            let request = Request {
                method: self.method,
                url: &self.url,
                headers: &self.headers,
                body: &self.body,
            };
            let response = self
                .fetcher
                .request(&request)
                .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
            self.response = Some(response);
        }
        Ok(self.response.as_mut().expect("just made"))
    }
}

/// The body of a request, written before its response is read.
struct Sent<'f>(Rc<RefCell<Exchange<'f>>>);

impl Write for Sent<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().body.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The headers, one `Name: value` line each, or the body of a response.
struct Received<'f> {
    exchange: Rc<RefCell<Exchange<'f>>>,
    headers: bool,
    data: Option<Vec<u8>>,
    pos: usize,
}

impl<'f> Received<'f> {
    fn new(exchange: &Rc<RefCell<Exchange<'f>>>, headers: bool) -> Received<'f> {
        Received {
            exchange: Rc::clone(exchange),
            headers,
            data: None,
            pos: 0,
        }
    }
}

impl BufRead for Received<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.data.is_none() {
            let mut exchange = self.exchange.borrow_mut();
            let response = exchange.response()?;
            self.data = Some(if self.headers {
                let lines = response.headers.iter().map(|(n, v)| format!("{n}: {v}\n"));
                lines.collect::<String>().into_bytes()
            } else {
                std::mem::take(&mut response.body)
            });
        }
        Ok(&self.data.as_deref().expect("just set")[self.pos..])
    }

    fn consume(&mut self, amount: usize) {
        self.pos += amount;
    }
}

impl Read for Received<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let available = self.fill_buf()?;
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}
