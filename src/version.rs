//! Versions and version constraints, ported from peipkg's
//! `internal/version` without change to the grammar, the ordering or the
//! constraint semantics.
//!
//! A version is `[<epoch>:]<upstream>-<revision>` (PSPU 5.5). Versions are
//! ordered by epoch, then upstream version, then revision (PSPU 5.6). A
//! constraint is one or more operator-and-version expressions joined by
//! commas, all of which must hold (PSPU 5.7).
//!
//! The ordering must agree with every other implementation on every pair of
//! valid versions (PSPU 5.5, Stability), so it follows peipkg exactly,
//! including its fixes for numeric pre-release segments and unbounded epochs
//! and revisions.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};

/// A parsed version string (PSPU 5.5): an optional epoch, an upstream
/// version, and a Peios revision. Obtain one through [`Version::parse`], or
/// [`Version::parse_relaxed`] where the revision may be omitted.
///
/// Equality and ordering are PSPU 5.6's, not the spelling's: `1.0~alpha-1`
/// equals `1.0~a-1`. That is why `Version` has no `Hash`, and why anything
/// keyed by its spelling, such as a [`PackageId`](crate::PackageId), compares
/// [`Version::as_str`] instead.
///
/// The epoch and revision have no maximum (PSPU 5.5), and two
/// implementations must agree on every pair of valid versions. Holding either
/// in an integer would make the accept and reject boundary depend on its
/// width, so both are kept as the canonical digit strings they were parsed
/// from and ordered as upstream numeric segments are.
#[derive(Debug, Clone)]
pub struct Version {
    raw: String,
    /// Canonical decimal; empty means the string carried none.
    epoch: String,
    upstream: String,
    /// Canonical decimal; empty when a relaxed version omits it.
    revision: String,
    /// The upstream version, tokenised, kept for comparison.
    segments: Vec<Segment>,
}

impl Version {
    /// Parse and validate a complete version string (PSPU 5.5): an optional
    /// `epoch:` prefix, an upstream version, and a required Peios revision.
    pub fn parse(s: &str) -> Result<Self> {
        parse(s, false)
    }

    /// Parse a version whose Peios revision may be omitted: a constraint
    /// operand (PSPU 5.7), or a provided capability's version (PSPU 5.21),
    /// which is a capability level rather than a packaging iteration, so
    /// `3.0` is well-formed. A revision, when present, is still parsed; when
    /// absent it is left empty. The upstream version is validated exactly as
    /// for [`Version::parse`].
    pub fn parse_relaxed(s: &str) -> Result<Self> {
        parse(s, true)
    }

    /// The epoch as a canonical decimal string: `0` if the string carried
    /// none.
    pub fn epoch(&self) -> &str {
        if self.epoch.is_empty() {
            "0"
        } else {
            &self.epoch
        }
    }

    /// The upstream version.
    pub fn upstream(&self) -> &str {
        &self.upstream
    }

    /// The Peios revision as a canonical decimal string, empty when a relaxed
    /// version omitted it.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The version string exactly as it was parsed.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

/// Parses a complete version, as [`Version::parse`] does.
impl FromStr for Version {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// The version string exactly as it was parsed.
impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Orders two versions (PSPU 5.6): by epoch, then by the upstream comparison
/// algorithm, then by revision.
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_epoch_upstream(self, other)
            .then_with(|| compare_numeric(&self.revision, &other.revision))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The same version under PSPU 5.6, which is not necessarily the same
/// spelling.
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

/// The shared parser. When `revision_optional` is true, as for a constraint
/// operand (PSPU 5.7), where `>= 3.0` is well-formed, a string with no
/// trailing `-<revision>` is accepted and the revision is left empty.
fn parse(s: &str, revision_optional: bool) -> Result<Version> {
    let mut epoch = String::new();
    let mut rest = s;
    if let Some((epoch_str, after)) = s.split_once(':') {
        epoch = parse_decimal(epoch_str)
            .map_err(|err| anyhow!("invalid epoch in '{s}': {err}"))?
            .to_owned();
        rest = after;
    }

    let mut revision = String::new();
    let mut upstream = rest;
    if let Some(idx) = rest.rfind('-') {
        let group = &rest[idx + 1..];
        match parse_decimal(group) {
            Ok(number) if number != "0" => {
                revision = number.to_owned();
                upstream = &rest[..idx];
            }
            // The trailing hyphen group is not a revision at all, so the whole
            // remainder is the upstream version: `1.0-rc1` is an upstream
            // version, not a version with revision `rc1`.
            //
            // PSPU 5.7 defines exactly one relaxation, that a constraint
            // operand may omit the revision, and says nothing about a group
            // that is present but malformed. Falling through for those too
            // would read `= 1.0-0`, a revision PSPU 5.5 reserves and forbids,
            // as "upstream literally equals `1.0-0`, any revision", which
            // matches nothing: the producer would get no diagnostic, and the
            // dependency would quietly become unsatisfiable.
            _ if revision_optional && has_non_digit(group) => {}
            Err(err) => bail!("invalid revision in '{s}': {err}"),
            Ok(_) => bail!("revision in '{s}' must be a positive integer"),
        }
    } else if !revision_optional {
        bail!("'{s}' has no -<revision>");
    }

    validate_upstream(upstream).map_err(|err| anyhow!("invalid upstream in '{s}': {err}"))?;
    Ok(Version {
        raw: s.to_owned(),
        epoch,
        upstream: upstream.to_owned(),
        revision,
        segments: tokenize(upstream),
    })
}

/// `s`, if it is a non-negative integer in canonical decimal form: ASCII
/// digits only and no leading zeros, zero being the single digit `0`. This is
/// the encoding PSPU 5.5 requires of the epoch and the revision.
///
/// It returns the digit string itself rather than an integer: PSPU 5.5 sets
/// no upper bound, so any fixed-width conversion would reject values the
/// grammar accepts.
fn parse_decimal(s: &str) -> Result<&str> {
    if s.is_empty() {
        bail!("empty number");
    }
    if !s.bytes().all(|c| c.is_ascii_digit()) {
        bail!("'{s}' is not a decimal number");
    }
    if s.len() > 1 && s.starts_with('0') {
        bail!("'{s}' has a leading zero");
    }
    Ok(s)
}

/// Whether `s` holds a byte that could not be part of a revision. An empty
/// group counts as all digits, so a trailing bare hyphen is reported as the
/// malformed revision it is rather than absorbed into the upstream version.
fn has_non_digit(s: &str) -> bool {
    !s.bytes().all(|c| c.is_ascii_digit())
}

/// Checks the upstream version against PSPU 5.5's character set and
/// structure.
fn validate_upstream(s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    let Some(&first) = bytes.first() else {
        bail!("empty upstream version");
    };
    if !first.is_ascii_alphanumeric() {
        bail!("must start with a letter or digit");
    }
    for &c in bytes {
        if !c.is_ascii_alphanumeric() && !b".+-~".contains(&c) {
            bail!("contains the invalid character {:?}", char::from(c));
        }
    }
    Ok(())
}

/// Whether a segment is a run of digits or a run of letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Numeric,
    Alphabetic,
}

/// One token of a tokenised upstream version (PSPU 5.6, Tokenising).
#[derive(Debug, Clone)]
struct Segment {
    kind: Kind,
    text: String,
    /// Whether the segment is part of the version's pre-release tail: it sits
    /// at or after the first `~` separator or the first recognized
    /// pre-release token.
    pre_release: bool,
}

/// Splits an upstream version into segments (PSPU 5.6, Tokenising) and marks
/// each as pre-release or not.
///
/// A segment is pre-release once the walk has passed either a `~` separator
/// or a recognized pre-release token; from that point every segment is
/// pre-release. A `-` is an ordinary separator: it does not itself begin a
/// pre-release tail. A `-` before `rc` produces a pre-release tail only
/// because `rc` is a recognized token.
///
/// `upstream` has passed [`validate_upstream`], so it is ASCII, and anything
/// that is not a separator or a digit is a letter.
fn tokenize(upstream: &str) -> Vec<Segment> {
    let bytes = upstream.as_bytes();
    let mut segments = Vec::new();
    let mut pre_release = false;

    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'~' => {
                // Segments after a tilde are pre-release.
                pre_release = true;
                i += 1;
            }
            b'.' | b'+' | b'-' => i += 1,
            c if c.is_ascii_digit() => {
                let end = run_end(bytes, i, u8::is_ascii_digit);
                segments.push(Segment {
                    kind: Kind::Numeric,
                    text: upstream[i..end].to_owned(),
                    pre_release,
                });
                i = end;
            }
            _ => {
                let end = run_end(bytes, i, u8::is_ascii_alphabetic);
                let text = &upstream[i..end];
                if pre_release_rank(text) < RANK_OTHER {
                    // A recognized pre-release token begins the tail.
                    pre_release = true;
                }
                segments.push(Segment {
                    kind: Kind::Alphabetic,
                    text: text.to_owned(),
                    pre_release,
                });
                i = end;
            }
        }
    }
    segments
}

/// The end of the run of bytes from `start` that satisfy `in_run`.
fn run_end(bytes: &[u8], start: usize, in_run: fn(&u8) -> bool) -> usize {
    bytes[start..]
        .iter()
        .position(|c| !in_run(c))
        .map_or(bytes.len(), |len| start + len)
}

/// The rank of an alphabetic segment that is not a recognized pre-release
/// token (PSPU 5.6, Pre-release rank).
const RANK_OTHER: u8 = 5;

/// The pre-release rank of an alphabetic segment (PSPU 5.6, Pre-release
/// rank). Recognized tokens rank 0 to 4, and anything else ranks
/// [`RANK_OTHER`]. Recognition is case-insensitive.
fn pre_release_rank(s: &str) -> u8 {
    match s.to_ascii_lowercase().as_str() {
        "dev" => 0,
        "alpha" | "a" => 1,
        "beta" | "b" => 2,
        "pre" => 3,
        "rc" => 4,
        _ => RANK_OTHER,
    }
}

/// Compares two versions by epoch and then by upstream version, ignoring the
/// revision. The constraint matcher uses it for operands whose revision was
/// omitted.
fn compare_epoch_upstream(a: &Version, b: &Version) -> Ordering {
    compare_numeric(&a.epoch, &b.epoch).then_with(|| compare_segments(&a.segments, &b.segments))
}

/// Compares two tokenised upstream versions (PSPU 5.6), including the rule
/// for unequal lengths when one is shorter.
fn compare_segments(a: &[Segment], b: &[Segment]) -> Ordering {
    let common = a.len().min(b.len());
    for (x, y) in a.iter().zip(b) {
        let c = compare_segment(x, y);
        if c != Ordering::Equal {
            return c;
        }
    }
    if a.len() == b.len() {
        return Ordering::Equal;
    }

    // One sequence continues past the other; its next segment decides.
    let (longer, shorter_is_a) = if b.len() > a.len() {
        (b, true)
    } else {
        (a, false)
    };
    let next = &longer[common];

    // The pre-release flag decides, not the kind. A tail that is a
    // pre-release is something the shorter version has already passed,
    // whether it is written `~rc1` or `~1`: gating this on the tail being
    // alphabetic made `1.0~1` sort above `1.0`, inverting the tilde for every
    // numeric snapshot form (`1.0~1`, `5.2~20240101`).
    let shorter_vs_longer = if next.pre_release {
        // A pre-release tail makes the shorter side greater.
        Ordering::Greater
    } else {
        // An ordinary tail makes the shorter side less.
        Ordering::Less
    };
    if shorter_is_a {
        shorter_vs_longer
    } else {
        shorter_vs_longer.reverse()
    }
}

/// Compares two individual segments (PSPU 5.6, Comparing two segments).
///
/// The pre-release flag is consulted before the kinds, because it is a
/// property of where the segment sits in the version rather than of what it
/// contains: a pre-release segment sorts below whatever follows the tail,
/// numeric or not. Reading it only when the kinds differ left `1.0~2` and
/// `1.0-2` indistinguishable, and indistinguishable is worse than misordered,
/// because equality under this order is identity.
fn compare_segment(a: &Segment, b: &Segment) -> Ordering {
    if a.pre_release != b.pre_release {
        return if a.pre_release {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    match (a.kind, b.kind) {
        (Kind::Numeric, Kind::Numeric) => compare_numeric(&a.text, &b.text),
        (Kind::Alphabetic, Kind::Alphabetic) => {
            let (ra, rb) = (pre_release_rank(&a.text), pre_release_rank(&b.text));
            if ra != rb {
                ra.cmp(&rb)
            } else if ra == RANK_OTHER {
                // The rank 5 tiebreak: ASCII byte order.
                a.text.as_bytes().cmp(b.text.as_bytes())
            } else {
                // The same recognized rank: aliases are equivalent.
                Ordering::Equal
            }
        }
        // A pre-release alphabetic segment is less than a numeric one, and an
        // ordinary one greater.
        (Kind::Numeric, Kind::Alphabetic) => {
            if b.pre_release {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Kind::Alphabetic, Kind::Numeric) => {
            if a.pre_release {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
    }
}

/// Compares two decimal digit strings as integers, ignoring leading zeros.
/// It works on the strings directly, so an arbitrarily long numeric segment
/// cannot overflow. Either may be empty, which compares as zero.
fn compare_numeric(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// A version constraint's relational operator (PSPU 5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equal,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
    NotEqual,
}

impl Operator {
    /// The operator as it is written.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::NotEqual => "!=",
        }
    }
}

/// One operator-and-version term of a constraint.
#[derive(Debug, Clone)]
struct Expression {
    op: Operator,
    operand: Version,
}

/// A parsed version constraint (PSPU 5.7): one or more operator-and-version
/// expressions combined with logical AND. The default constraint has none, so
/// it imposes no restriction and matches every version; it cannot be parsed,
/// only constructed.
#[derive(Debug, Clone, Default)]
pub struct Constraint {
    exprs: Vec<Expression>,
}

impl Constraint {
    /// Parse a constraint string (PSPU 5.7): comma-separated expressions,
    /// each an optional operator followed by a version, combined with logical
    /// AND. A bare version with no operator means `=`. Whitespace around
    /// operators and commas is ignored.
    ///
    /// An operand may omit the `-<revision>`, as in `>= 3.0`; it is then
    /// matched on epoch and upstream version only.
    pub fn parse(s: &str) -> Result<Self> {
        let mut exprs = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                bail!("constraint '{s}' has an empty expression");
            }
            let expr =
                parse_expression(part).map_err(|err| anyhow!("invalid constraint '{s}': {err}"))?;
            exprs.push(expr);
        }
        Ok(Self { exprs })
    }

    /// Whether `v` satisfies the constraint: every expression must hold. The
    /// default constraint matches every version.
    pub fn matches(&self, v: &Version) -> bool {
        self.exprs.iter().all(|expr| expr.matches(v))
    }

    /// Whether this is the default constraint, matching every version.
    pub fn is_any(&self) -> bool {
        self.exprs.is_empty()
    }

    /// Whether the constraint can require a version older than the newest
    /// available. A constraint with only lower bounds is satisfied by the
    /// newest version if by any; an exact, upper-bound or exclusion
    /// constraint may need an older one.
    pub fn may_need_historical_versions(&self) -> bool {
        self.exprs.iter().any(|expr| {
            matches!(
                expr.op,
                Operator::Equal | Operator::Less | Operator::LessEqual | Operator::NotEqual
            )
        })
    }

    /// The constraint in the form a manifest records: the canonical
    /// [`Display`](fmt::Display) form, except that the default constraint is
    /// the empty string, which is how a manifest says "any version".
    ///
    /// This is not the `Display` form, which writes the default constraint as
    /// the word `any`. Writing that into a manifest would change its meaning:
    /// `any` parses as `= any`, which matches only an upstream version
    /// spelled `any`.
    pub fn to_manifest_string(&self) -> String {
        if self.is_any() {
            String::new()
        } else {
            self.to_string()
        }
    }

    /// The constraint as an inventory writes it: each expression as one token
    /// of its operator and operand with no space between, such as `>=8.22`.
    /// The default constraint has no tokens.
    pub(crate) fn tokens(&self) -> Vec<String> {
        (self.exprs.iter())
            .map(|expr| format!("{}{}", expr.op.as_str(), expr.operand))
            .collect()
    }

    /// The constraint `tokens` write, as [`Constraint::tokens`] gives them.
    /// No tokens is the default constraint.
    pub(crate) fn from_tokens(tokens: &[&str]) -> Result<Self> {
        if tokens.is_empty() {
            return Ok(Self::default());
        }
        Self::parse(&tokens.join(", "))
    }

    /// The constraint a manifest's string records: the default constraint
    /// for the empty string, and [`Constraint::parse`] for anything else.
    pub fn from_manifest_string(s: &str) -> Result<Self> {
        if s.is_empty() {
            Ok(Self::default())
        } else {
            Self::parse(s)
        }
    }
}

/// Parses a constraint, as [`Constraint::parse`] does.
impl FromStr for Constraint {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// The constraint in a canonical form that parses back to it: each
/// expression as its operator, a space and its operand as written, joined by
/// `, `. The default constraint is the word `any`, which does not parse.
impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.exprs.is_empty() {
            return f.write_str("any");
        }
        for (i, expr) in self.exprs.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{} {}", expr.op.as_str(), expr.operand)?;
        }
        Ok(())
    }
}

/// Parses one operator-and-version term. Two-character operators are checked
/// before their one-character prefixes, so that `>=` is not read as `>`.
fn parse_expression(part: &str) -> Result<Expression> {
    const OPERATORS: [(&str, Operator); 6] = [
        (">=", Operator::GreaterEqual),
        ("<=", Operator::LessEqual),
        ("!=", Operator::NotEqual),
        (">", Operator::Greater),
        ("<", Operator::Less),
        ("=", Operator::Equal),
    ];
    let (op, operand) = OPERATORS
        .iter()
        .find_map(|&(text, op)| part.strip_prefix(text).map(|rest| (op, rest)))
        .unwrap_or((Operator::Equal, part));
    let operand = parse(operand.trim(), true)?;
    Ok(Expression { op, operand })
}

impl Expression {
    /// Whether `v` satisfies this expression. When the operand omitted its
    /// revision, the revision is not part of the comparison.
    fn matches(&self, v: &Version) -> bool {
        let mut c = compare_epoch_upstream(v, &self.operand);
        if c == Ordering::Equal && !self.operand.revision.is_empty() {
            c = compare_numeric(&v.revision, &self.operand.revision);
        }
        match self.op {
            Operator::Equal => c.is_eq(),
            Operator::Greater => c.is_gt(),
            Operator::GreaterEqual => c.is_ge(),
            Operator::Less => c.is_lt(),
            Operator::LessEqual => c.is_le(),
            Operator::NotEqual => c.is_ne(),
        }
    }
}
