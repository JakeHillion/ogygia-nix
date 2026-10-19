//! Grisu2 (Loitsch, "Printing Floating-Point Numbers Quickly and Accurately
//! with Integers", PLDI 2010), which Nix's JSON writer uses to print floats.
//! Its output always reads back as the same float but is not always the
//! shortest that does, so neither Rust's formatting nor ryu can stand in.

/// A float as `f * 2^e`.
#[derive(Clone, Copy)]
struct Fp {
    f: u64,
    e: i32,
}

impl Fp {
    fn sub(self, o: Fp) -> Fp {
        Fp {
            f: self.f - o.f,
            e: self.e,
        }
    }

    /// The upper 64 bits of the product, rounded to nearest.
    fn mul(self, o: Fp) -> Fp {
        let p = u128::from(self.f) * u128::from(o.f) + (1 << 63);
        Fp {
            f: (p >> 64) as u64,
            e: self.e + o.e + 64,
        }
    }

    fn normalize(self) -> Fp {
        let s = self.f.leading_zeros();
        Fp {
            f: self.f << s,
            e: self.e - s as i32,
        }
    }
}

const ALPHA: i32 = -60;
const GAMMA: i32 = -32;

/// `10^k` for every eighth `k`, as normalised significand, binary exponent
/// and `k`.
const POWERS: [(u64, i32, i32); 87] = [
    (0xFA8FD5A0081C0288, -1220, -348),
    (0xBAAEE17FA23EBF76, -1193, -340),
    (0x8B16FB203055AC76, -1166, -332),
    (0xCF42894A5DCE35EA, -1140, -324),
    (0x9A6BB0AA55653B2D, -1113, -316),
    (0xE61ACF033D1A45DF, -1087, -308),
    (0xAB70FE17C79AC6CA, -1060, -300),
    (0xFF77B1FCBEBCDC4F, -1034, -292),
    (0xBE5691EF416BD60C, -1007, -284),
    (0x8DD01FAD907FFC3C, -980, -276),
    (0xD3515C2831559A83, -954, -268),
    (0x9D71AC8FADA6C9B5, -927, -260),
    (0xEA9C227723EE8BCB, -901, -252),
    (0xAECC49914078536D, -874, -244),
    (0x823C12795DB6CE57, -847, -236),
    (0xC21094364DFB5637, -821, -228),
    (0x9096EA6F3848984F, -794, -220),
    (0xD77485CB25823AC7, -768, -212),
    (0xA086CFCD97BF97F4, -741, -204),
    (0xEF340A98172AACE5, -715, -196),
    (0xB23867FB2A35B28E, -688, -188),
    (0x84C8D4DFD2C63F3B, -661, -180),
    (0xC5DD44271AD3CDBA, -635, -172),
    (0x936B9FCEBB25C996, -608, -164),
    (0xDBAC6C247D62A584, -582, -156),
    (0xA3AB66580D5FDAF6, -555, -148),
    (0xF3E2F893DEC3F126, -529, -140),
    (0xB5B5ADA8AAFF80B8, -502, -132),
    (0x87625F056C7C4A8B, -475, -124),
    (0xC9BCFF6034C13053, -449, -116),
    (0x964E858C91BA2655, -422, -108),
    (0xDFF9772470297EBD, -396, -100),
    (0xA6DFBD9FB8E5B88F, -369, -92),
    (0xF8A95FCF88747D94, -343, -84),
    (0xB94470938FA89BCF, -316, -76),
    (0x8A08F0F8BF0F156B, -289, -68),
    (0xCDB02555653131B6, -263, -60),
    (0x993FE2C6D07B7FAC, -236, -52),
    (0xE45C10C42A2B3B06, -210, -44),
    (0xAA242499697392D3, -183, -36),
    (0xFD87B5F28300CA0E, -157, -28),
    (0xBCE5086492111AEB, -130, -20),
    (0x8CBCCC096F5088CC, -103, -12),
    (0xD1B71758E219652C, -77, -4),
    (0x9C40000000000000, -50, 4),
    (0xE8D4A51000000000, -24, 12),
    (0xAD78EBC5AC620000, 3, 20),
    (0x813F3978F8940984, 30, 28),
    (0xC097CE7BC90715B3, 56, 36),
    (0x8F7E32CE7BEA5C70, 83, 44),
    (0xD5D238A4ABE98068, 109, 52),
    (0x9F4F2726179A2245, 136, 60),
    (0xED63A231D4C4FB27, 162, 68),
    (0xB0DE65388CC8ADA8, 189, 76),
    (0x83C7088E1AAB65DB, 216, 84),
    (0xC45D1DF942711D9A, 242, 92),
    (0x924D692CA61BE758, 269, 100),
    (0xDA01EE641A708DEA, 295, 108),
    (0xA26DA3999AEF774A, 322, 116),
    (0xF209787BB47D6B85, 348, 124),
    (0xB454E4A179DD1877, 375, 132),
    (0x865B86925B9BC5C2, 402, 140),
    (0xC83553C5C8965D3D, 428, 148),
    (0x952AB45CFA97A0B3, 455, 156),
    (0xDE469FBD99A05FE3, 481, 164),
    (0xA59BC234DB398C25, 508, 172),
    (0xF6C69A72A3989F5C, 534, 180),
    (0xB7DCBF5354E9BECE, 561, 188),
    (0x88FCF317F22241E2, 588, 196),
    (0xCC20CE9BD35C78A5, 614, 204),
    (0x98165AF37B2153DF, 641, 212),
    (0xE2A0B5DC971F303A, 667, 220),
    (0xA8D9D1535CE3B396, 694, 228),
    (0xFB9B7CD9A4A7443C, 720, 236),
    (0xBB764C4CA7A44410, 747, 244),
    (0x8BAB8EEFB6409C1A, 774, 252),
    (0xD01FEF10A657842C, 800, 260),
    (0x9B10A4E5E9913129, 827, 268),
    (0xE7109BFBA19C0C9D, 853, 276),
    (0xAC2820D9623BF429, 880, 284),
    (0x80444B5E7AA7CF85, 907, 292),
    (0xBF21E44003ACDD2D, 933, 300),
    (0x8E679C2F5E44FF8F, 960, 308),
    (0xD433179D9C8CB841, 986, 316),
    (0x9E19DB92B4E31BA9, 1013, 324),
    (0xEB96BF6EBADF77D9, 1039, 332),
    (0xAF87023B9BF0EE6B, 1066, 340),
];

/// The digits and decimal exponent of a positive finite `v`: `v` is
/// approximately `digits * 10^exp`.
pub fn grisu2(v: f64) -> (Vec<u8>, i32) {
    let bits = v.to_bits();
    let frac = bits & ((1 << 52) - 1);
    let biased = (bits >> 52) as i32;
    let (f, e) = if biased == 0 {
        (frac, -1074)
    } else {
        (frac | (1 << 52), biased - 1075)
    };
    let plus = Fp {
        f: (f << 1) + 1,
        e: e - 1,
    }
    .normalize();
    let minus = if frac == 0 && biased > 1 {
        Fp {
            f: (f << 2) - 1,
            e: e - 2,
        }
    } else {
        Fp {
            f: (f << 1) - 1,
            e: e - 1,
        }
    };
    let minus = Fp {
        f: minus.f << (minus.e - plus.e),
        e: plus.e,
    };
    let w = Fp { f, e }.normalize();
    debug_assert_eq!(w.e, plus.e);

    let &(cf, ce, ck) = POWERS
        .iter()
        .find(|&&(_, ce, _)| ce + plus.e + 64 >= ALPHA)
        .expect("a cached power is in range");
    debug_assert!(ce + plus.e + 64 <= GAMMA);
    let c = Fp { f: cf, e: ce };
    let w = w.mul(c);
    let mut lo = minus.mul(c);
    let mut hi = plus.mul(c);
    lo.f += 1;
    hi.f -= 1;

    let (digits, kappa) = digit_gen(w, lo, hi);
    (digits, kappa - ck)
}

/// The shortest digits of a number in `[lo, hi]`, rounded towards `w`, and
/// the power of ten of the last.
fn digit_gen(w: Fp, lo: Fp, hi: Fp) -> (Vec<u8>, i32) {
    let mut delta = hi.sub(lo).f;
    let mut dist = hi.sub(w).f;
    let shift = -hi.e as u32;
    let one = 1u64 << shift;
    let mut p1 = (hi.f >> shift) as u32;
    let mut p2 = hi.f & (one - 1);
    let mut digits = Vec::with_capacity(17);

    let mut kappa = p1.checked_ilog10().map_or(1, |n| n + 1) as i32;
    let mut div = 10u32.pow(kappa as u32 - 1);
    while kappa > 0 {
        digits.push(b'0' + (p1 / div) as u8);
        p1 %= div;
        kappa -= 1;
        let rest = (u64::from(p1) << shift) + p2;
        if rest <= delta {
            round(&mut digits, dist, delta, rest, u64::from(div) << shift);
            return (digits, kappa);
        }
        div /= 10;
    }
    loop {
        p2 *= 10;
        delta *= 10;
        dist *= 10;
        digits.push(b'0' + (p2 >> shift) as u8);
        p2 &= one - 1;
        kappa -= 1;
        if p2 <= delta {
            round(&mut digits, dist, delta, p2, one);
            return (digits, kappa);
        }
    }
}

/// Moves the last digit down while that stays in range and brings the
/// number closer to `w`, `dist` above it.
fn round(digits: &mut [u8], dist: u64, delta: u64, mut rest: u64, ten_k: u64) {
    let last = digits.last_mut().expect("at least one digit");
    while rest < dist
        && delta - rest >= ten_k
        && (rest + ten_k < dist || dist - rest > rest + ten_k - dist)
    {
        *last -= 1;
        rest += ten_k;
    }
}
