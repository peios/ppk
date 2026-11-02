//! Versions and constraints. Ported one for one from peipkg's
//! `internal/version` tests, each named after the Go test it ports, with the
//! same vectors, so that parity is visible.

use std::cmp::Ordering;

use ppk::{Constraint, Version};

fn must_parse(s: &str) -> Version {
    Version::parse(s).unwrap_or_else(|err| panic!("parse '{s}': {err}"))
}

fn must_constraint(s: &str) -> Constraint {
    Constraint::parse(s).unwrap_or_else(|err| panic!("parse constraint '{s}': {err}"))
}

/// `a` compared with `b`, as peipkg's `Compare`'s -1, 0 or 1.
fn compare(a: &str, b: &str) -> i32 {
    match must_parse(a).cmp(&must_parse(b)) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

// version_test.go

#[test]
fn parse_valid() {
    let cases = [
        ("1.26.2-3", "0", "1.26.2", "3"),
        // Upstream may itself contain hyphens.
        ("1.26.2-rc.1-1", "0", "1.26.2-rc.1", "1"),
        ("2:0.5.0-1", "2", "0.5.0", "1"),
        ("0.22-1", "0", "0.22", "1"),
        // An explicit epoch of zero is well-formed.
        ("0:1.0-1", "0", "1.0", "1"),
        ("16beta1-1", "0", "16beta1", "1"),
        ("1.0~rc.1-42", "0", "1.0~rc.1", "42"),
        // PSPU 5.5 sets no maximum: neither of these fits an i32, and the
        // larger does not fit an i64 either.
        (
            "4294967296:1.0-2147483648",
            "4294967296",
            "1.0",
            "2147483648",
        ),
        (
            "1.0-99999999999999999999",
            "0",
            "1.0",
            "99999999999999999999",
        ),
    ];
    for (input, epoch, upstream, revision) in cases {
        let v = must_parse(input);
        assert_eq!(v.epoch(), epoch, "{input}: epoch");
        assert_eq!(v.upstream(), upstream, "{input}: upstream");
        assert_eq!(v.revision(), revision, "{input}: revision");
        assert_eq!(v.to_string(), input, "{input}: a verbatim round trip");
    }
}

#[test]
fn parse_invalid() {
    let cases = [
        ("empty", ""),
        ("no revision", "1.0"),
        ("empty revision", "1.0-"),
        ("empty upstream", "-1"),
        ("revision is zero", "1.0-0"),
        ("revision has a leading zero", "1.0-01"),
        ("epoch has a leading zero", "01:1.0-1"),
        ("empty epoch", ":1.0-1"),
        ("non-numeric epoch", "x:1.0-1"),
        ("upstream starts with a separator", ".5-1"),
        ("space in the version", "1.0 -1"),
        ("invalid upstream character", "1@0-1"),
        ("non-numeric revision", "1.0-1.5"),
        (
            "revision-less (constraint form, not a full version)",
            "1.0-rc.1",
        ),
        ("epoch with no version", "2:"),
    ];
    for (name, input) in cases {
        assert!(
            Version::parse(input).is_err(),
            "{name}: parse '{input}' should have failed"
        );
    }
}

/// PSPU 5.7 defines exactly one relaxation, that a constraint operand may
/// omit the revision, and says nothing about a trailing group that is present
/// but malformed. Falling through for those too reinterpreted `= 1.0-0` as
/// "upstream literally equals `1.0-0`, any revision", which matches nothing:
/// the producer got no diagnostic and the dependency quietly became
/// unsatisfiable (PEI-421).
#[test]
fn parse_relaxed_rejects_a_malformed_revision() {
    for s in [
        // PSPU 5.5 reserves and forbids revision 0.
        "1.0-0",  // A leading zero.
        "1.0-01", // Present but empty.
        "1.0-",
    ] {
        assert!(
            Version::parse_relaxed(s).is_err(),
            "parse_relaxed('{s}') accepted a malformed revision"
        );
        // The strict path has always rejected these; relaxed agrees.
        assert!(
            Version::parse(s).is_err(),
            "parse('{s}') accepted a malformed revision"
        );
    }
}

/// The relaxation itself still works: an omitted revision is fine, and a
/// trailing group that is not a number at all is part of the upstream
/// version rather than a broken revision.
#[test]
fn parse_relaxed_still_accepts_the_relaxation() {
    for (s, want_upstream) in [
        ("3.0", "3.0"),
        ("1.0-rc1", "1.0-rc1"),
        ("1.0~rc1", "1.0~rc1"),
        // A real revision is still parsed as one.
        ("1.0-2", "1.0"),
    ] {
        let v = Version::parse_relaxed(s).unwrap_or_else(|err| panic!("'{s}': {err}"));
        assert_eq!(v.upstream(), want_upstream, "parse_relaxed('{s}')");
    }
}

// compare_test.go

/// The worked examples of PSPU 5.6, and its unequal-length and aliasing
/// examples, as full version strings: a `-1` revision is appended where the
/// specification writes a bare upstream version. The third value is the
/// expected comparison of the first with the second.
const COMPARE_VECTORS: &[(&str, &str, i32)] = &[
    // PSPU 5.6's worked examples.
    // Identical.
    ("1.0-1", "1.0-1", 0),
    // A numeric segment differs.
    ("1.0-1", "2.0-1", -1),
    // Numeric, not lexical.
    ("1.10-1", "1.9-1", 1),
    // The longer continues numerically.
    ("1.0-1", "1.0.1-1", -1),
    // The longer continues with a pre-release.
    ("1.0-1", "1.0-rc.1-1", 1),
    // Numeric within a pre-release.
    ("1.0-rc.1-1", "1.0-rc.2-1", -1),
    // Pre-release rank alpha (1) < beta (2).
    ("1.0-alpha-1", "1.0-beta-1", -1),
    // Pre-release rank rc (4) > pre (3).
    ("1.0-rc-1", "1.0-pre-1", 1),
    // Numeric within a concatenated pre-release.
    ("1.0a1-1", "1.0a2-1", -1),
    // Pre-release rank a (1) < b (2).
    ("1.0a1-1", "1.0b1-1", -1),
    // The tilde forces a pre-release.
    ("1.0~rc1-1", "1.0-1", -1),
    // The epoch dominates.
    ("0:1.0-1", "1:0.5-1", -1),
    // The Peios revision differs.
    ("1.0-1", "1.0-2", -1),
    // `foo` is not a pre-release token (rank 5).
    ("1.0-foo-1", "1.0-1", 1),
    // Unequal lengths: a pre-release alphabetic tail makes the longer side
    // older.
    ("1.0-1", "1.0.alpha-1", 1),
    // Aliasing: recognized pre-release tokens compare equal.
    ("1.0~alpha-1", "1.0~a-1", 0),
    ("1.0~beta-1", "1.0~b-1", 0),
    // Recognition is case-insensitive.
    ("1.0~Alpha-1", "1.0~ALPHA-1", 0),
    // The rank 5 tiebreak is ASCII byte order.
    ("1.0-foo-1", "1.0-zzz-1", -1),
    // Epoch and revision.
    ("5:1.0-1", "5:1.0-1", 0),
    // The revision is compared numerically, not lexically.
    ("1.0-9", "1.0-10", -1),
];

#[test]
fn compare_vectors() {
    for &(a, b, want) in COMPARE_VECTORS {
        assert_eq!(compare(a, b), want, "compare('{a}', '{b}')");
        // Comparison must be antisymmetric.
        assert_eq!(compare(b, a), -want, "compare('{b}', '{a}') (antisymmetry)");
    }
}

#[test]
fn equal_and_less() {
    let older = must_parse("1.0-1");
    let newer = must_parse("2.0-1");

    assert!(older < newer, "1.0-1 should order before 2.0-1");
    assert!(newer >= older, "2.0-1 should not order before 1.0-1");
    assert!(older == must_parse("1.0-1"), "1.0-1 should equal 1.0-1");
    assert!(older != newer, "1.0-1 should not equal 2.0-1");
    // Pre-release token aliases are equivalent.
    assert!(
        must_parse("1.0~alpha-1") == must_parse("1.0~a-1"),
        "1.0~alpha-1 should equal 1.0~a-1"
    );
}

/// Ordering is a usable total order: a scrambled but known ascending
/// sequence sorts back into place. peipkg shuffles with a seeded generator;
/// this uses fixed permutations, which test the same thing without one.
#[test]
fn sort_order() {
    let ascending = [
        "1.0~dev-1",
        "1.0~alpha-1",
        "1.0~beta-1",
        "1.0~pre-1",
        "1.0~rc-1",
        "1.0-1",
        "1.0-2",
        "1.0.1-1",
        "1.1-1",
        "2.0-1",
        "1:0.1-1",
    ];
    let len = ascending.len();
    for step in [3, 5, 7] {
        let mut parsed: Vec<Version> = (0..len)
            .map(|i| must_parse(ascending[(i * step) % len]))
            .collect();
        parsed.sort();
        let got: Vec<String> = parsed.iter().map(ToString::to_string).collect();
        assert_eq!(got, ascending, "sorted with stride {step}");
    }
    let mut reversed: Vec<Version> = ascending.iter().rev().map(|s| must_parse(s)).collect();
    reversed.sort();
    let got: Vec<&str> = reversed.iter().map(Version::as_str).collect();
    assert_eq!(got, ascending, "sorted from reversed");
}

// constraint_test.go

#[test]
fn parse_constraint_valid() {
    for s in [
        ">= 3.0-1",
        ">=3.0-1, <4.0-1",
        // A bare version is an `=` constraint.
        "1.26.2-3",
        "= 1.0-1",
        "!= 2.0-1",
        // PSPU 5.7: a constraint operand may omit the revision.
        ">= 3.0",
        // Whitespace around operators and commas is ignored.
        "> 1.0-1 , < 2.0-1",
    ] {
        if let Err(err) = Constraint::parse(s) {
            panic!("parse constraint '{s}': {err}");
        }
    }
}

#[test]
fn parse_constraint_invalid() {
    for s in [
        // No expression at all.
        "",
        // An operator with no version.
        ">=",
        // An empty expression.
        ">= , < 4.0-1",
        // The operand is not a valid version.
        ">= not!aversion",
        // A trailing comma leaves an empty expression.
        "3.0-1,",
    ] {
        assert!(
            Constraint::parse(s).is_err(),
            "parse constraint '{s}' should have failed"
        );
    }
}

#[test]
fn constraint_matches() {
    let cases = [
        (">= 3.0-1", "3.5-1", true),
        (">= 3.0-1", "3.0-1", true),
        (">= 3.0-1", "2.9-1", false),
        (">= 3.0-1, < 4.0-1", "3.5-1", true),
        (">= 3.0-1, < 4.0-1", "4.0-1", false),
        (">= 3.0-1, < 4.0-1", "2.0-1", false),
        // A bare version pins exactly, including the revision.
        ("1.26.2-3", "1.26.2-3", true),
        ("1.26.2-3", "1.26.2-4", false),
        ("!= 2.0-1", "2.1-1", true),
        ("!= 2.0-1", "2.0-1", false),
        // A revision-less operand ignores the revision.
        (">= 3.0", "3.0-1", true),
        (">= 3.0", "3.5-9", true),
        (">= 3.0", "2.9-1", false),
        // `= 3.0` matches any revision of upstream 3.0.
        ("= 3.0", "3.0-1", true),
        ("= 3.0", "3.0-99", true),
        ("= 3.0", "3.1-1", false),
    ];
    for (constraint, version, want) in cases {
        let got = must_constraint(constraint).matches(&must_parse(version));
        assert_eq!(got, want, "('{constraint}').matches('{version}')");
    }
}

#[test]
fn zero_constraint_matches_everything() {
    let unconstrained = Constraint::default();
    for s in ["1.0-1", "99:0.0.0-1", "1.0~rc-1"] {
        assert!(
            unconstrained.matches(&must_parse(s)),
            "the default constraint should match '{s}'"
        );
    }
}

#[test]
fn constraint_may_need_historical_versions() {
    let cases = [
        ("", false),
        (">= 1.0", false),
        ("> 1.0-1", false),
        ("1.0-1", true),
        ("= 1.0", true),
        ("< 2.0", true),
        ("<= 2.0-1", true),
        (">= 1.0, < 2.0", true),
        ("!= 1.0-1", true),
    ];
    for (constraint, want) in cases {
        let c = if constraint.is_empty() {
            Constraint::default()
        } else {
            must_constraint(constraint)
        };
        assert_eq!(
            c.may_need_historical_versions(),
            want,
            "('{constraint}').may_need_historical_versions()"
        );
    }
}

#[test]
fn constraint_string() {
    let c = must_constraint(">=3.0-1,<4.0-1");
    assert_eq!(c.to_string(), ">= 3.0-1, < 4.0-1");
    assert_eq!(Constraint::default().to_string(), "any");
}

// prerelease_numeric_test.go

/// PEI-388: a numeric pre-release segment was invisible to the comparison,
/// so `1.0~1` sorted above the release it precedes. Every Debian-style
/// numeric snapshot form was affected.
#[test]
fn numeric_pre_release_segments_sort_below_the_release() {
    let cases = [
        // The reported inversions.
        ("1.0~1-1", "1.0-1", -1),
        ("1.0~2-1", "1.0.1-1", -1),
        ("5.2~20240101-1", "5.2-1", -1),
        ("3.0~2-1", "3.0-1", -1),
        // The equality cases, which were the more damaging half: equality
        // under this order is identity.
        ("1.0~foo-1", "1.0-foo-1", -1),
        ("1.0~2-1", "1.0-2-1", -1),
        // Ordering within a numeric pre-release tail still works.
        ("1.0~1-1", "1.0~2-1", -1),
        ("1.0~1-1", "1.0~1-2", -1),
        // A numeric pre-release sorts above an alphabetic one, as the rule
        // for mixed kinds has always had it for the non-tilde forms.
        ("1.0~rc-1", "1.0~1-1", -1),
        // Unaffected cases, asserted so that the fix is visibly narrow.
        ("1.0-1", "1.0-1", 0),
        ("1.0-1", "1.0.1-1", -1),
        ("1.0~rc1-1", "1.0-1", -1),
        ("1.0-foo-1", "1.0-1", 1),
        ("1.0~alpha-1", "1.0~a-1", 0),
        ("1.0-alpha-1", "1.0-beta-1", -1),
        ("1.0a1-1", "1.0b1-1", -1),
    ];
    for (a, b, want) in cases {
        assert_eq!(compare(a, b), want, "compare('{a}', '{b}')");
        assert_eq!(compare(b, a), -want, "compare('{b}', '{a}') (asymmetry)");
    }
}

// unbounded_test.go

/// PSPU 5.5 says the epoch is a non-negative integer and the revision a
/// positive integer, neither with a maximum, and its stability clause
/// requires any two conforming implementations to agree on every pair of
/// valid version strings. Holding either in a fixed-width integer would break
/// both.
#[test]
fn epoch_and_revision_are_not_bounded_by_the_platform_int() {
    // Every one of these exceeds an i64.
    for s in [
        "1.0-99999999999999999999",
        "99999999999999999999:1.0-1",
        "340282366920938463463374607431768211456:1.0-2",
    ] {
        if let Err(err) = Version::parse(s) {
            panic!("parse '{s}': {err}; the grammar sets no maximum");
        }
    }
    // The i32 and i64 boundaries, accepted alike.
    for s in [
        "1.0-2147483647",
        "1.0-2147483648",
        "1.0-2147483649",
        "1.0-9223372036854775807",
        "1.0-9223372036854775808",
    ] {
        if let Err(err) = Version::parse(s) {
            panic!("parse '{s}': {err}");
        }
    }
}

/// Ordering stays numeric right through the range where an integer would
/// have overflowed or wrapped.
#[test]
fn oversized_epochs_and_revisions_still_order_numerically() {
    let cases = [
        // Revisions either side of the i64 ceiling.
        ("1.0-9223372036854775807", "1.0-9223372036854775808", -1),
        ("1.0-9223372036854775808", "1.0-9223372036854775807", 1),
        // A longer digit string is the larger number, not the lesser.
        ("1.0-99999999999999999999", "1.0-100000000000000000000", -1),
        ("1.0-2", "1.0-10", -1),
        // Epochs, which dominate the upstream version entirely.
        ("9223372036854775807:1.0-1", "9223372036854775808:0.1-1", -1),
        (
            "99999999999999999999:1.0-1",
            "99999999999999999999:1.0-1",
            0,
        ),
        // Equal values of differing width cannot occur, since a leading zero
        // is refused, so equality is exact string equality.
        ("1.0-99999999999999999999", "1.0-99999999999999999999", 0),
    ];
    for (a, b, want) in cases {
        assert_eq!(compare(a, b), want, "compare('{a}', '{b}')");
    }
}

// ppk's own

/// The manifest form of a constraint: the empty string is the default
/// constraint, both ways, and anything else is the canonical form.
#[test]
fn constraint_manifest_string() {
    assert_eq!(Constraint::default().to_manifest_string(), "");
    assert!(Constraint::from_manifest_string("").unwrap().is_any());
    assert_eq!(
        must_constraint("1.0-1").to_manifest_string(),
        "= 1.0-1",
        "a bare version is written with its `=`"
    );
    assert_eq!(
        Constraint::from_manifest_string(">=3.0,<4.0")
            .unwrap()
            .to_manifest_string(),
        ">= 3.0, < 4.0"
    );
    // `any` is only how the default constraint displays; read back, it is a
    // bare version, which is an `=` constraint.
    let any = Constraint::from_manifest_string("any").unwrap();
    assert!(!any.is_any());
    assert_eq!(any.to_manifest_string(), "= any");
}
