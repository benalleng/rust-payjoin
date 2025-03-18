use std::any::{Any, TypeId};

use bitcoin::SignedAmount;
use bitcoin_uri::Param;
use honggfuzz::fuzz;
use payjoin::{Uri, UriExt, Url};

fn do_test(data: &[u8]) {
    let data_str = String::from_utf8_lossy(data);
    let pj_uri = match data_str.parse::<Uri<_>>() {
        Ok(pj_uri) => pj_uri.assume_checked(),
        Err(_) => return,
    };
    let address = pj_uri.address.is_spend_standard();
    if !address {
        return;
    }
    let signed_amount = match pj_uri.amount.unwrap().to_string().parse::<SignedAmount>() {
        Ok(amt) => amt,
        Err(_) => return,
    };
    if let Some(label) = pj_uri.clone().label {
        if TypeId::of::<Param>() != label.type_id() {
            return;
        }
    };
    if let Some(message) = pj_uri.clone().message {
        if TypeId::of::<Param>() != message.type_id() {
            return;
        }
    };
    let extras = pj_uri.clone().check_pj_supported().unwrap().extras;
    assert_eq!(pj_uri.to_string(), data_str);
    assert!(SignedAmount::MAX_MONEY > signed_amount && signed_amount > SignedAmount::ZERO);
    assert!(TypeId::of::<bool>() == extras.is_output_substitution_disabled().type_id());
    assert!(TypeId::of::<Url>() == extras.endpoint().type_id())
}

fn main() {
    loop {
        fuzz!(|data| {
            do_test(data);
        });
    }
}

#[cfg(all(test, fuzzing))]
mod tests {
    fn extend_vec_from_hex(hex: &str, out: &mut Vec<u8>) {
        let mut b = 0;
        for (idx, c) in hex.as_bytes().iter().enumerate() {
            b <<= 4;
            match *c {
                b'A'..=b'F' => b |= c - b'A' + 10,
                b'a'..=b'f' => b |= c - b'a' + 10,
                b'0'..=b'9' => b |= c - b'0',
                _ => panic!("Bad hex"),
            }
            if (idx & 1) == 1 {
                out.push(b);
                b = 0;
            }
        }
    }

    #[test]
    fn duplicate_crash() {
        let mut a = Vec::new();
        extend_vec_from_hex("00000000", &mut a);
        super::do_test(&a);
    }
}
