#[path = "../probe.rs"]
mod probe;
use probe::*;
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const ATOMS: &[&str] = &[
    "a",
    "b",
    "word ",
    " ",
    "  ",
    "\n",
    "\n",
    "\n\n",
    "\n\n",
    "\t",
    "\r",
    "\r\n",
    "\0",
    "*",
    "**",
    "_",
    "__",
    "~~",
    "~",
    "`",
    "``",
    "```",
    "```\n",
    "~~~\n",
    "    ",
    "[",
    "]",
    "(",
    ")",
    "](",
    "](/x)",
    "](https://a.example)",
    "](javascript:x)",
    "![",
    "[r]",
    "[r][r]",
    "[r]: /u \"t\"\n",
    "<",
    ">",
    "<a>",
    "</a>",
    "<a href=\"x\">",
    "<b>",
    "<div>",
    "</div>",
    "<div>\n",
    "<pre>",
    "</pre>",
    "<table>",
    "<tr>",
    "<td>",
    "<p>",
    "</p>",
    "<!--",
    "-->",
    "<![CDATA[",
    "]]>",
    "<?",
    "?>",
    "<script>",
    "</script>",
    "<https://a.example>",
    "<a@b.c>",
    "<javascript:x>",
    "> ",
    "> ",
    ">",
    "- ",
    "- ",
    "* ",
    "+ ",
    "1. ",
    "7. ",
    "1) ",
    "# ",
    "## ",
    "###### ",
    "---",
    "---\n",
    "===\n",
    "***\n",
    "|",
    "|",
    " | ",
    "|---|",
    "|-|-|",
    "| a | b |\n",
    "|:-:|--:|\n",
    "\\",
    "\\|",
    "\\\n",
    "  \n",
    "&amp;",
    "&#0;",
    "&#x3c;",
    "&nbsp;",
    "&",
    "'",
    "\"",
    "é",
    "\u{200b}",
    "\u{202e}",
    "\u{a0}",
    "\u{2028}",
    "\u{feff}",
    "\u{c}",
    "\u{b}",
    "\u{1}",
    "[^1]",
    "[^1]: n\n",
    "- [ ] ",
    "$x$",
    "{#id}",
    ":--",
    "Term\n: def\n",
    "http://bare.example ",
    "www.x.example ",
];

const INLINE: &[&str] = &[
    "a",
    "b ",
    " ",
    "*",
    "*",
    "**",
    "**",
    "_",
    "__",
    "~~",
    "~~",
    "`",
    "``",
    "[",
    "]",
    "](/x)",
    "](https://a.example \"t\")",
    "](javascript:x)",
    "![",
    "[r]",
    "<b>",
    "</b>",
    "<a href=x>",
    "<https://a.example>",
    "<a@b.c>",
    "\\",
    "\\|",
    "&amp;",
    "&#0;",
    "<!--",
    "-->",
    "  ",
    "é",
    "\u{1}",
    "(",
    ")",
    "'",
    "\"",
    "<",
    ">",
];
fn inline(rng: &mut Rng, s: &mut String, max: usize) {
    for _ in 0..rng.below(max) {
        s.push_str(INLINE[rng.below(INLINE.len())]);
    }
}
fn doc(rng: &mut Rng) -> String {
    let mode = std::env::var("MODE").unwrap_or_default();
    if mode == "table" {
        let mut s = String::new();
        let depth = if rng.below(3) == 0 {
            22 + rng.below(12)
        } else {
            rng.below(4)
        };
        let kinds: Vec<&str> = (0..depth)
            .map(|_| ["> ", "- ", "1. "][rng.below(3)])
            .collect();
        let first: String = kinds.concat();
        let cont: String = kinds
            .iter()
            .map(|k| match *k {
                "> " => "> ",
                "- " => "  ",
                _ => "   ",
            })
            .collect();
        if rng.below(4) == 0 {
            s.push_str(&first);
            inline(rng, &mut s, 6);
            s.push('\n');
            s.push_str(&cont);
        } else {
            s.push_str(&first);
        }
        let cols = 1 + rng.below(4);
        for r in 0..(2 + rng.below(4)) {
            if r > 0 {
                s.push_str(&cont);
            }
            if r == 1 {
                for _ in 0..cols {
                    s.push_str(["|---", "|:-:", "|--:"][rng.below(3)]);
                }
                s.push_str("|\n");
                continue;
            }
            let c = if rng.below(4) == 0 {
                rng.below(7)
            } else {
                cols
            };
            if rng.below(8) != 0 {
                s.push('|');
            }
            for _ in 0..c {
                inline(rng, &mut s, 8);
                s.push('|');
            }
            s.push('\n');
        }
        if rng.below(2) == 0 {
            s.push_str(&cont);
            inline(rng, &mut s, 8);
        }
        if rng.below(3) == 0 {
            s.push_str("\n\n");
            s.push_str(&cont);
            inline(rng, &mut s, 8);
        }
        return s;
    }
    if mode == "inline" {
        let mut s = String::new();
        if rng.below(4) == 0 {
            for _ in 0..(26 + rng.below(8)) {
                s.push_str(["> ", "- "][rng.below(2)])
            }
        }
        if rng.below(5) == 0 {
            s.push_str("# ");
        }
        inline(rng, &mut s, 60);
        if rng.below(3) == 0 {
            s.push_str("\n\n[r]: /u 't'\n");
        }
        return s;
    }
    let mut s = String::new();
    // sometimes start deep, to probe the depth limit
    match rng.below(6) {
        0 => {
            let n = 24 + rng.below(12);
            let p = ["> ", "- ", "1. "][rng.below(3)];
            for _ in 0..n {
                s.push_str(p)
            }
        }
        1 => {
            let n = 24 + rng.below(12);
            for _ in 0..n {
                s.push_str(["> ", "- ", "1. "][rng.below(3)])
            }
        }
        _ => {}
    }
    let prefix = s.clone();
    let n = 1 + rng.below(40);
    for _ in 0..n {
        let a = ATOMS[rng.below(ATOMS.len())];
        s.push_str(a);
        if a.ends_with('\n') && rng.below(3) == 0 {
            // continue inside the same containers
            s.push_str(&prefix.replace("- ", "  ").replace("1. ", "   "));
        }
    }
    s
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20000);
    let seed: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x9E3779B97F4A7C15);
    let mut rng = Rng(seed);
    let (mut model_bad, mut parse_bad, mut slow) = (0, 0, 0);
    let mut cov = std::collections::BTreeMap::<String, usize>::new();
    let mut shown = std::collections::BTreeMap::<String, usize>::new();
    for i in 0..n {
        let src = doc(&mut rng);
        let t0 = Instant::now();
        let html = render(&src);
        let dt = t0.elapsed();
        if dt.as_millis() > 200 {
            slow += 1;
            println!("SLOW {dt:?} {src:?}");
        }
        let ours = preprocess(tokens(&html));
        for t in &ours {
            if let Tok::Start(n, _) = t {
                *cov.entry(n.clone()).or_default() += 1;
            }
        }
        if let Some(why) = model(&ours) {
            model_bad += 1;
            let key = why.split(' ').take(3).collect::<Vec<_>>().join(" ");
            let c = shown.entry(format!("model: {key}")).or_default();
            *c += 1;
            if *c <= 3 {
                println!("MODEL #{i}: {why}\n  src: {src:?}\n  html: {html}");
            }
        }
        let theirs = preprocess(tokens(&browser(&html)));
        if ours != theirs {
            parse_bad += 1;
            let at = ours
                .iter()
                .zip(&theirs)
                .position(|(a, b)| a != b)
                .unwrap_or(ours.len().min(theirs.len()));
            let key = format!(
                "parse: {:?} vs {:?}",
                ours.get(at).map(kind),
                theirs.get(at).map(kind)
            );
            let c = shown.entry(key).or_default();
            *c += 1;
            if *c <= 3 {
                println!(
                    "PARSE #{i} at token {at}: ours {:?} / parsed {:?}\n  src: {src:?}\n  html: {html}\n  parsed: {}",
                    ours.get(at),
                    theirs.get(at),
                    browser(&html)
                );
            }
        }
    }
    println!(
        "docs {n}: model violations {model_bad}, parser changes {parse_bad}, slow {slow}"
    );
    for (k, v) in shown {
        println!("  {v:6} {k}");
    }
    println!("{cov:?}");
}
fn kind(t: &Tok) -> String {
    match t {
        Tok::Start(n, _) => format!("<{n}>"),
        Tok::End(n) => format!("</{n}>"),
        Tok::Text(_) => "text".into(),
        Tok::Comment(c) => {
            format!("<!--{}-->", c.trim_end_matches(char::is_numeric))
        }
    }
}
