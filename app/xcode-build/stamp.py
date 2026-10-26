# Sets the version keys of an Info.plist in place, keeping the format
# xcodebuild wrote it in: XML for macOS bundles, binary for iOS ones.
import plistlib
import sys

path, version, build, revision = sys.argv[1:]
with open(path, "rb") as f:
    data = f.read()
fmt = plistlib.FMT_BINARY if data.startswith(b"bplist") else plistlib.FMT_XML
info = plistlib.loads(data)
keys = {
    "CFBundleShortVersionString": version,
    "CFBundleVersion": build,
    "OgygiaGitRevision": revision,
}
for key in keys:
    if key not in info:
        sys.exit(f"{path} has no {key} to stamp")
info.update(keys)
with open(path, "wb") as f:
    plistlib.dump(info, f, fmt=fmt, sort_keys=False)
