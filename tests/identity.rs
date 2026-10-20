use ppk::Identity;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn identity_of_bytes_is_their_sha256() {
    let identity = Identity::of(b"");
    assert_eq!(identity.hex(), EMPTY_SHA256);
    assert_eq!(identity.to_string(), format!("sha256:{EMPTY_SHA256}"));
}

#[test]
fn display_parses_back() {
    let identity = Identity::of(b"hello");
    assert_eq!(identity.to_string().parse::<Identity>().unwrap(), identity);
}

#[test]
fn only_the_canonical_spelling_parses() {
    let bad = [
        String::new(),
        EMPTY_SHA256.to_owned(),
        format!("sha256:{}", EMPTY_SHA256.to_uppercase()),
        format!("sha256:{}", &EMPTY_SHA256[1..]),
        format!("sha256:{EMPTY_SHA256}0"),
        format!("sha256:{EMPTY_SHA256}\n"),
        format!("SHA256:{EMPTY_SHA256}"),
        format!("sha512:{EMPTY_SHA256}"),
        "sha256:../../etc/passwd".to_owned(),
    ];
    for s in bad {
        let err = s.parse::<Identity>().unwrap_err();
        assert_eq!(err.to_string(), format!("'{s}' is not a sha256 identity"));
    }
}
