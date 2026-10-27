"""NDM's own language file: every UI string in NDM's source (src/*.rs) -> Languages/ndm_en.lng.

Reads only NDM's own code, never NDM's files, so the result is NDM's to publish. A translation is a
copy of ndm_en.lng named ndm_xx.lng with its `lang=` line changed and the right side of each line
translated; NDM lists it under View -> Language (src/i18n.rs, `own`).

    python tools/ndm_lng.py           # write Languages/ndm_en.lng
    python tools/ndm_lng.py --check   # exit 1 if the file is out of date (new UI strings)
"""
import glob, os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
OUT = os.path.join(ROOT, "Languages", "ndm_en.lng")

LIT = r'"((?:[^"\\]|\\.)+)"'
# the calls whose string arguments the UI shows (same set the translation audit uses, in every file)
CALLS = re.compile(r'(?:trf\(|tr_os\(|tr\(&?|d2?\.(?:label|label_r|text|button|check|check_off|radio|radio_off|group|page_header|readonly)\([^"\n]*?|listview\([^"\n]*?|tabs\([^"\n]*?|\(\s*)' + LIT)
# literals that pass the pattern but are not UI text (headers, file names, formats, examples)
NOT_UI = re.compile(
    r"bmp$|png$|^User-Agent$|^Accept$|^Content-Length$|^X-NDM$|\[File\]|Hot$|CARGO|ndman|bundled|me:p|not a url|rundll|shell32|=$"
    r"|^<local>$|h:p;|^about:|^\*\.zip;|^HTTP/|^chrome-extension|^moz-extension|\.chm$|\.exe$|\.lng$|\.json$|^https?://|^%[A-Z]+%"
    # protocol and file-format tokens, HTTP headers and methods, all-caps names, formats, examples
    r"|^#EXT|^[A-Z][a-z]+(-[A-Z][a-z]+)+:?$|^[A-Z0-9_-]+:?$|^\d{3} [A-Z]|%[aHMSbY]|&amp;|^0x|^/n /d|^\d|^key\(s\) key$"
    # registry values, window classes, fonts, header and folder names used in code
    r"|^</?script|^(AppsUseLightTheme|Arial|Authorization|Chromium|Cookie|Mozilla|NDMDropTarget|NewDownloadManager"
    r"|ProxyEnable|ProxyOverride|ProxyServer|Range|Referer|UniformResourceLocatorW|WWW-Authenticate|Fonts|Languages)$"
)


def unescape(s):
    """A Rust string literal's text, with newlines as the file's \\n."""
    s = s.replace("\\\\", "\x02").replace('\\"', '"').replace("\\'", "'").replace("\\r\\n", "\\n").replace("\\t", "\t")
    return s.replace("\x02", "\\\\")


def strings():
    found = set()
    for f in sorted(glob.glob(os.path.join(ROOT, "src", "*.rs"))):
        src = open(f, encoding="utf-8").read()
        src = src.split("#[cfg(test)]")[0]  # tests aren't UI
        src = "\n".join(l for l in src.split("\n") if not l.lstrip().startswith("//"))  # nor are comments
        for m in CALLS.finditer(src):
            s = unescape(m.group(1))
            if re.search("[A-Za-z]{2}", s) and "{" not in s and "=" not in s and not re.match(r"^[a-z_:./*\\-]+$", s) and not NOT_UI.search(s):
                found.add(s)
    return sorted(found, key=lambda s: (s.lower(), s))


def render(items):
    head = [
        "lang=0x409 English",
        "// New Download Manager language file (UTF-8). Generated from NDM's source by",
        "// tools/ndm_lng.py; written for NDM and free to use under NDM's license.",
        "// To translate: copy to ndm_xx.lng, change the lang= line (language ID and name),",
        "// translate the text after each '=', keep %s / %d / %ld and \\n. Lines starting // are comments.",
    ]
    # the right side is what NDM shows: NDM's wording carries NDM's name, as `i18n::brand` does at run time
    shown = lambda s: s.replace("New Download Manager", "New Download Manager").replace("NDM", "NDM")
    return "\n".join(head + [f"{s}={shown(s)}" for s in items]) + "\n"


if __name__ == "__main__":
    text = render(strings())
    if "--check" in sys.argv:
        same = os.path.exists(OUT) and open(OUT, encoding="utf-8").read() == text
        print("ndm_en.lng is up to date" if same else "ndm_en.lng is out of date: run tools/ndm_lng.py")
        sys.exit(0 if same else 1)
    open(OUT, "w", encoding="utf-8", newline="\n").write(text)
    print(f"{OUT}: {text.count(chr(10)) - 5} strings")
