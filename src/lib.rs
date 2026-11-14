//! SigmaOS-makepkg: Arch PKGBUILD parser and package-build model.
//!
//! Real implementation: parses PKGBUILD bash-assignment syntax into a
//! validated `Pkgbuild` struct (pkgname, pkgver, pkgrel, arch, sources
//! with integrity hashes, depends arrays, install file list, options),
//! validates Arch packaging rules, and renders back deterministically.
//! Replaces the generic `Tool` placeholder.

use std::collections::BTreeMap;
use std::fmt;

/// Valid package architectures (Arch supports any; common subset listed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Any, X86_64, X86_64_V3, Aarch64, Armv7h, Riscv64,
}

impl Arch {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "any" => Some(Self::Any), "x86_64" => Some(Self::X86_64),
            "x86_64_v3" => Some(Self::X86_64_V3), "aarch64" => Some(Self::Aarch64),
            "armv7h" => Some(Self::Armv7h), "riscv64" => Some(Self::Riscv64),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any", Self::X86_64 => "x86_64", Self::X86_64_V3 => "x86_64_v3",
            Self::Aarch64 => "aarch64", Self::Armv7h => "armv7h", Self::Riscv64 => "riscv64",
        }
    }
}

/// A source entry: URL plus optional integrity hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub url: String,
    pub sha256: Option<String>,
    pub sha512: Option<String>,
    pub b2: Option<String>,
    pub local: bool, // file:// or bare filename already in dir
}

/// Build options (Arch `options=()` array).
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub debug: bool,         // !debug → strip
    pub lto: bool,           // lto
    pub docs: bool,          // docs
    pub emptydirs: bool,     // emptydirs
    pub zipman: bool,        // zipman
    pub purge: bool,         // purge
    pub ccache: bool,        // ccache
    pub staticlibs: bool,    // staticlibs
}

impl Options {
    fn parse(list: &[String]) -> Self {
        let mut o = Self::default();
        for s in list {
            match s.as_str() {
                "debug" => o.debug = true,
                "lto" => o.lto = true,
                "docs" => o.docs = true,
                "emptydirs" => o.emptydirs = true,
                "zipman" => o.zipman = true,
                "purge" => o.purge = true,
                "ccache" => o.ccache = true,
                "staticlibs" => o.staticlibs = true,
                _ => {}
            }
        }
        o
    }
    fn negated(list: &[String]) -> Vec<String> {
        // '!debug' style entries → strip '!'
        list.iter().filter_map(|s| s.strip_prefix('!').map(String::from)).collect()
    }
}

/// Parsed and validated PKGBUILD.
#[derive(Debug, Clone)]
pub struct Pkgbuild {
    pub pkgname: String,
    pub pkgbase: Option<String>,
    pub pkgver: String,
    pub pkgrel: String,          // e.g. "1" or "1.1"
    pub epoch: Option<u32>,
    pub pkgdesc: String,
    pub arch: Vec<Arch>,
    pub url: String,
    pub license: Vec<String>,
    pub source: Vec<Source>,
    pub depends: Vec<String>,
    pub makedepends: Vec<String>,
    pub checkdepends: Vec<String>,
    pub optdepends: Vec<(String, String)>, // (pkg, description)
    pub backup: Vec<String>,
    pub options: Options,
    pub install: Option<String>,  // .install script name
    pub groups: Vec<String>,
}

/// Full package version string: [epoch:]pkgver-pkgrel.
pub fn full_version(p: &Pkgbuild) -> String {
    let base = format!("{}-{}", p.pkgver, p.pkgrel);
    match p.epoch {
        Some(e) => format!("{}:{}", e, base),
        None => base,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgError {
    MissingField(&'static str),
    BadField { field: &'static str, value: String, reason: String },
    NoArch,
    DuplicateSource(String),
    BadSourceUrl(String),
    SourceHashMismatch { url: String },
    BadPkgname(String),
    BadPkgver(String),
    BadPkgrel(String),
}

impl fmt::Display for PkgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(f2) => write!(f, "missing required field {}", f2),
            Self::BadField { field, value, reason } =>
                write!(f, "bad {} {:?}: {}", field, value, reason),
            Self::NoArch => write!(f, "arch array is empty or invalid"),
            Self::DuplicateSource(u) => write!(f, "duplicate source {}", u),
            Self::BadSourceUrl(u) => write!(f, "unparseable source URL {}", u),
            Self::SourceHashMismatch { url } =>
                write!(f, "source {} has conflicting hashes", url),
            Self::BadPkgname(n) => write!(f, "bad pkgname {:?}", n),
            Self::BadPkgver(v) => write!(f, "bad pkgver {:?}", v),
            Self::BadPkgrel(r) => write!(f, "bad pkgrel {:?}", r),
        }
    }
}

impl std::error::Error for PkgError {}

/// Arch pkgname rules: lowercase alnum + @ . _ + - ; no leading -/. ; max sensible.
fn valid_pkgname(n: &str) -> bool {
    !n.is_empty() && n.len() <= 255
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()
            || matches!(c, '@' | '.' | '_' | '+' | '-'))
        && !n.starts_with('-') && !n.starts_with('.')
        && !n.ends_with('-') && !n.ends_with('.')
}

/// pkgver: alphanumeric + . _ + ~ (no '-'; dash separates pkgrel).
fn valid_pkgver(v: &str) -> bool {
    !v.is_empty() && v.len() <= 128
        && v.chars().all(|c| c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | '+' | '~'))
}

/// pkgrel: positive integer, optionally dotted (1, 1.1).
fn valid_pkgrel(r: &str) -> bool {
    !r.is_empty()
        && r.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Parse a single-quoted or bare bash array literal: ("a" "b" 'c').
fn parse_array(body: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let quote = c;
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n == quote { break; }
                    cur.push(n);
                }
                out.push(std::mem::take(&mut cur));
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

/// Extract `key=(...)` or `key=value` assignments from PKGBUILD text.
/// Returns ordered (key, raw_value) pairs; functions like pkgver() are
/// captured as raw strings (real makepkg evals them — we store verbatim).
fn extract_assignments(text: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut in_array: Option<(String, String)> = None; // (key, buffer)
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('!') || line.starts_with("function") {
            continue;
        }
        if let Some((k, buf)) = in_array.as_mut() {
            if line.contains(')') {
                buf.push(' ');
                buf.push_str(line.trim_end_matches(')').trim());
                out.push((k.clone(), std::mem::take(buf)));
                in_array = None;
            } else {
                buf.push(' ');
                buf.push_str(line);
            }
            continue;
        }
        // key=(...) possibly multi-line
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim().to_string();
            let val = line[eq+1..].trim().to_string();
            if val.starts_with('(') {
                if val.ends_with(')') && val.len() > 2 {
                    out.push((key, val[1..val.len()-1].to_string()));
                } else {
                    in_array = Some((key, val[1..].to_string()));
                }
            } else {
                out.push((key, val));
            }
        }
    }
    out
}

/// Parse PKGBUILD text into a validated Pkgbuild.
pub fn parse(text: &str) -> Result<Pkgbuild, PkgError> {
    let assigns = extract_assignments(text);
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in assigns {
        map.entry(k).or_default().push(v);
    }
    let get = |k: &str| map.get(k).and_then(|v| v.last()).cloned();
    let arr = |k: &str| -> Vec<String> {
        map.get(k).map(|v| v.last().map(|s| parse_array(s)).unwrap_or_default())
            .unwrap_or_default()
    };

    let pkgname = get("pkgname").ok_or(PkgError::MissingField("pkgname"))?;
    let pkgname = pkgname.trim_matches(|c| c == '"' || c == '\'').to_string();
    if !valid_pkgname(&pkgname) {
        return Err(PkgError::BadPkgname(pkgname));
    }
    let pkgver = get("pkgver").ok_or(PkgError::MissingField("pkgver"))?;
    let pkgver = pkgver.trim_matches(|c| c == '"' || c == '\'').to_string();
    if !valid_pkgver(&pkgver) {
        return Err(PkgError::BadPkgver(pkgver));
    }
    let pkgrel = get("pkgrel").ok_or(PkgError::MissingField("pkgrel"))?;
    let pkgrel = pkgrel.trim_matches(|c| c == '"' || c == '\'').to_string();
    if !valid_pkgrel(&pkgrel) {
        return Err(PkgError::BadPkgrel(pkgrel));
    }
    let epoch = get("epoch").and_then(|e| e.trim().parse::<u32>().ok());

    let arch_strs = arr("arch");
    if arch_strs.is_empty() {
        return Err(PkgError::NoArch);
    }
    let mut arch = vec![];
    for a in &arch_strs {
        let a = a.trim_matches(|c| c == '"' || c == '\'');
        arch.push(Arch::parse(a).ok_or_else(|| PkgError::BadField {
            field: "arch", value: a.into(), reason: "unknown arch".into() })?);
    }

    // sources: merge source[] + *sums[] arrays positionally
    let src_urls = arr("source");
    let sha256s = arr("sha256sums");
    let sha512s = arr("sha512sums");
    let b2s = arr("b2sums");
    let mut source = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for (i, u) in src_urls.iter().enumerate() {
        let u = u.trim_matches(|c| c == '"' || c == '\'').to_string();
        if u.is_empty() { continue; }
        if !seen.insert(u.clone()) {
            return Err(PkgError::DuplicateSource(u));
        }
        // url sanity: must contain :// or be a local filename
        let local = !u.contains("://") && !u.starts_with("http");
        if !local && !u.contains("://") {
            return Err(PkgError::BadSourceUrl(u));
        }
        // skip "skip" hash entries
        let pick = |v: &Vec<String>| -> Option<String> {
            v.get(i).and_then(|h| {
                let h = h.trim();
                if h == "SKIP" || h.is_empty() { None } else { Some(h.to_string()) }
            })
        };
        source.push(Source {
            url: u,
            sha256: pick(&sha256s),
            sha512: pick(&sha512s),
            b2: pick(&b2s),
            local,
        });
    }

    // optdepends: "pkg: description" entries
    let optdepends = arr("optdepends").iter().filter_map(|s| {
        s.split_once(':').map(|(p, d)| {
            (p.trim().trim_matches(|c| c=='"'||c=='\'').to_string(),
             d.trim().trim_matches(|c| c=='"'||c=='\'').to_string())
        })
    }).collect();

    let options_raw: Vec<String> = arr("options")
        .iter().map(|s| s.trim_matches(|c| c=='"'||c=='\'').to_string()).collect();

    Ok(Pkgbuild {
        pkgname,
        pkgbase: get("pkgbase").map(|s| s.trim_matches(|c| c=='"'||c=='\'').to_string()),
        pkgver, pkgrel, epoch,
        pkgdesc: get("pkgdesc").unwrap_or_default()
            .trim_matches(|c| c=='"'||c=='\'').to_string(),
        arch, url: get("url").unwrap_or_default()
            .trim_matches(|c| c=='"'||c=='\'').to_string(),
        license: arr("license"),
        source,
        depends: arr("depends"),
        makedepends: arr("makedepends"),
        checkdepends: arr("checkdepends"),
        optdepends,
        backup: arr("backup"),
        options: Options::parse(&Options::negated(&options_raw).into_iter()
            .chain(options_raw.iter().filter(|s| !s.starts_with('!')).cloned()).collect::<Vec<_>>()),
        install: get("install").map(|s| s.trim_matches(|c| c=='"'||c=='\'').to_string()),
        groups: arr("groups"),
    })
}

/// Render a Pkgbuild back to PKGBUILD text (deterministic order).
pub fn render(p: &Pkgbuild) -> String {
    let arr = |v: &[String]| -> String {
        if v.is_empty() { "()".into() } else {
            format!("({})", v.iter().map(|s| format!("\"{}\"", s)).collect::<Vec<_>>().join(" "))
        }
    };
    let opt = {
        let mut o = vec![];
        if p.options.lto { o.push("lto".to_string()); }
        if p.options.docs { o.push("docs".to_string()); }
        if p.options.emptydirs { o.push("emptydirs".to_string()); }
        if p.options.zipman { o.push("zipman".to_string()); }
        if p.options.ccache { o.push("ccache".to_string()); }
        if p.options.staticlibs { o.push("staticlibs".to_string()); }
        arr(&o)
    };
    let mut out = String::new();
    out.push_str(&format!("pkgname=\"{}\"\n", p.pkgname));
    if let Some(b) = &p.pkgbase { out.push_str(&format!("pkgbase=\"{}\"\n", b)); }
    out.push_str(&format!("pkgver=\"{}\"\n", p.pkgver));
    out.push_str(&format!("pkgrel={}\n", p.pkgrel));
    if let Some(e) = p.epoch { out.push_str(&format!("epoch={}\n", e)); }
    out.push_str(&format!("pkgdesc=\"{}\"\n", p.pkgdesc));
    let archs: Vec<String> = p.arch.iter().map(|a| a.as_str().to_string()).collect();
    out.push_str(&format!("arch={}\n", arr(&archs)));
    out.push_str(&format!("url=\"{}\"\n", p.url));
    out.push_str(&format!("license={}\n", arr(&p.license)));
    let srcs: Vec<String> = p.source.iter().map(|s| format!("\"{}\"", s.url)).collect();
    out.push_str(&format!("source={}\n", arr(&srcs)));
    let s256: Vec<String> = p.source.iter()
        .map(|s| s.sha256.clone().unwrap_or_else(|| "SKIP".into())).collect();
    out.push_str(&format!("sha256sums={}\n", arr(&s256)));
    if !p.depends.is_empty() { out.push_str(&format!("depends={}\n", arr(&p.depends))); }
    if !p.optdepends.is_empty() {
        let v: Vec<String> = p.optdepends.iter()
            .map(|(a,b)| format!("\"{}: {}\"", a, b)).collect();
        out.push_str(&format!("optdepends={}\n", arr(&v)));
    }
    if !p.backup.is_empty() { out.push_str(&format!("backup={}\n", arr(&p.backup))); }
    out.push_str(&format!("options={}\n", opt));
    if let Some(i) = &p.install { out.push_str(&format!("install={}\n", i)); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# Maintainer: Test
pkgname="hello-world"
pkgbase="hello"
pkgver=1.2.3
pkgrel=1
epoch=2
pkgdesc="A test package"
arch=('x86_64' 'aarch64')
url="https://example.com/hello"
license=('MIT')
source=("https://example.com/hello-1.2.3.tar.gz"
        "https://example.com/hello-data.tar.xz")
sha256sums=('aaa111' 'SKIP')
sha512sums=('bbb222' 'ccc333')
depends=('glibc' 'zlib: for compression')
optdepends=('gtk3: for GUI' 'qt5: alt GUI')
backup=('etc/hello.conf')
options=('lto' '!debug' 'docs')
install=hello.install
"#;

    #[test]
    fn test_parse_full_pkgbuild_aaa() {
        // Arrange & Act
        let p = parse(SAMPLE).unwrap();
        // Assert
        assert_eq!(p.pkgname, "hello-world");
        assert_eq!(p.pkgver, "1.2.3");
        assert_eq!(p.pkgrel, "1");
        assert_eq!(p.epoch, Some(2));
        assert_eq!(full_version(&p), "2:1.2.3-1");
        assert_eq!(p.arch.len(), 2);
        assert_eq!(p.source.len(), 2);
        assert_eq!(p.source[0].sha256.as_deref(), Some("aaa111"));
        assert_eq!(p.source[1].sha256, None); // SKIP → None
        assert_eq!(p.source[1].sha512.as_deref(), Some("ccc333"));
        assert_eq!(p.optdepends.len(), 2);
        assert!(p.options.lto && p.options.docs);
        assert_eq!(p.install.as_deref(), Some("hello.install"));
    }

    #[test]
    fn test_missing_required_fields_aaa() {
        // Arrange & Act & Assert
        assert!(matches!(parse("pkgver=1\npkgrel=1\narch=('x86_64')\n"),
            Err(PkgError::MissingField("pkgname"))));
        assert!(matches!(parse("pkgname=x\npkgrel=1\narch=('x86_64')\n"),
            Err(PkgError::MissingField("pkgver"))));
    }

    #[test]
    fn test_no_arch_rejected_aaa() {
        // Arrange & Act & Assert
        let bad = "pkgname=x\npkgver=1\npkgrel=1\narch=()\n";
        assert!(matches!(parse(bad), Err(PkgError::NoArch)));
    }

    #[test]
    fn test_bad_names_rejected_aaa() {
        // Arrange & Act & Assert
        let mk = |name: &str| format!(
            "pkgname={}\npkgver=1\npkgrel=1\narch=('x86_64')\n", name);
        assert!(parse(&mk("Bad-Name")).is_err());       // uppercase
        assert!(parse(&mk("-leading")).is_err());
        assert!(parse(&mk("trailing-")).is_err());
        assert!(parse(&mk("ok_name+1.2")).is_ok());
        // bad pkgver with dash
        let bv = "pkgname=x\npkgver=1-2\npkgrel=1\narch=('x86_64')\n";
        assert!(matches!(parse(bv), Err(PkgError::BadPkgver(_))));
        // bad pkgrel
        let br = "pkgname=x\npkgver=1\npkgrel=abc\narch=('x86_64')\n";
        assert!(matches!(parse(br), Err(PkgError::BadPkgrel(_))));
    }

    #[test]
    fn test_duplicate_source_rejected_aaa() {
        // Arrange
        let dup = r#"pkgname=x
pkgver=1
pkgrel=1
arch=('x86_64')
source=("https://a.com/f.tar.gz" "https://a.com/f.tar.gz")
"#;
        // Act & Assert
        assert!(matches!(parse(dup), Err(PkgError::DuplicateSource(_))));
    }

    #[test]
    fn test_local_source_flagged_aaa() {
        // Arrange
        let local = r#"pkgname=x
pkgver=1
pkgrel=1
arch=('x86_64')
source=("local-patch.diff" "https://a.com/s.tar.gz")
"#;
        // Act
        let p = parse(local).unwrap();
        // Assert
        assert!(p.source[0].local);
        assert!(!p.source[1].local);
    }

    #[test]
    fn test_render_roundtrip_aaa() {
        // Arrange
        let p = parse(SAMPLE).unwrap();
        // Act
        let text = render(&p);
        let back = parse(&text).unwrap();
        // Assert
        assert_eq!(back.pkgname, p.pkgname);
        assert_eq!(back.pkgver, p.pkgver);
        assert_eq!(back.source.len(), p.source.len());
        assert_eq!(full_version(&back), full_version(&p));
    }

    #[test]
    fn test_multiline_array_parsed_aaa() {
        // Arrange: array spanning lines
        let multi = r#"pkgname=x
pkgver=1
pkgrel=1
arch=('x86_64')
source=(
  "https://a.com/1.tar.gz"
  "https://a.com/2.tar.gz"
)
depends=(
  'foo'
  "bar"
)
"#;
        // Act
        let p = parse(multi).unwrap();
        // Assert
        assert_eq!(p.source.len(), 2);
        assert_eq!(p.depends, vec!["foo", "bar"]);
    }

    #[test]
    fn test_empty_minimal_pkgbuild_aaa() {
        // Arrange: only required fields
        let min = "pkgname=min\npkgver=0.1\npkgrel=1\narch=('any')\n";
        // Act
        let p = parse(min).unwrap();
        // Assert
        assert_eq!(p.arch, vec![Arch::Any]);
        assert!(p.source.is_empty());
        assert_eq!(full_version(&p), "0.1-1");
    }
}
