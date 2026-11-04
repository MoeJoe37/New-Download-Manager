"""NDM's own language file: every UI string in NDM's source (src/**/*.rs) -> Languages/ndm_en.lng.

Reads only NDM's own code, so the result is NDM's to publish. A translation is a
copy of ndm_en.lng named ndm_xx.lng with its `lang=` line changed and the right side of each line
translated; NDM lists it under View -> Language (src/i18n.rs, `own`).

    python tools/ndm_lng.py           # write Languages/ndm_en.lng
    python tools/ndm_lng.py --check   # exit 1 if the file is out of date (new UI strings), or a translation
                                      # lacks a string or breaks its placeholders
"""
import glob, os, re, sys
from pathlib import Path

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
OUT = os.path.join(ROOT, "Languages", "ndm_en.lng")

LIT = r'"((?:[^"\\]|\\.)+)"'
# the calls whose string arguments the UI shows (same set the translation audit uses, in every file)
CALLS = (r'(?:trf\(|tr_os\(|tr\(&?|d2?\.(?:label|label_r|text|button|check|check_off|radio|radio_off|group|page_header|readonly)\([^"\n]*?|listview\([^"\n]*?|tabs\([^"\n]*?|\(\s*'
                   # tables of UI text: a tuple's later strings, tuples that start with a value, arrays, menu items
                   r'|(?<=")\s*,\s*|\(\s*(?:(?:true|false|-?\d+(?:\.\d+)?|[A-Za-z_][\w:]*),\s*){1,4}|\[\s*|item\(ui,\s*'
                   # message windows: Dialog::Info { title: "...", text: "..." }; texts picked in if / match arms
                   r'|\b(?:title|text): |\{\s*(?="[^"\n]*"\s*\})|=>\s*)' + LIT)
DLU_LOCALS = re.compile(r"\blet\s+(?:mut\s+)?(\w+)\s*=\s*(?:dlu::)?D::new(?:_offset)?\(")
TEST_ONLY_SOURCES = {"tests.rs", "test_support.rs", "transport_bench.rs"}

# literals that pass the pattern but are not UI text (headers, file names, formats, examples)
NOT_UI = re.compile(
    r"bmp$|png$|^User-Agent$|^Accept$|^Content-Length$|^X-NDM$|^\[File\]$|Hot$|CARGO|bundled|me:p|not a url|rundll|shell32|=$"
    r"|^<local>$|h:p;|^about:|^\*\.zip;|^HTTP/|^chrome-extension|^moz-extension|\.chm$|\.exe$|\.lng$|\.json$|^https?://|^%[A-Z]+%"
    # protocol and file-format tokens, HTTP headers and methods, all-caps names, formats, examples
    r"|^#EXT|^[A-Z][a-z]+(-[A-Z][a-z]+)+:?$|^[A-Z0-9_-]+:?$|^\d{3} [A-Z]|%[aHMSbY]|&amp;|^0x|^/n /d|^\d[\d.:]*$|^key\(s\) key$"
    # registry values, window classes, fonts, header and folder names used in code
    r"|^</?script|^(AppsUseLightTheme|Arial|Tahoma|Authorization|Chromium|Cookie|Mozilla|NDMDropTarget|NewDownloadManager"
    r"|ProxyEnable|ProxyOverride|ProxyServer|Range|Referer|UniformResourceLocatorW|WWW-Authenticate|Fonts|Languages)$"
    # paths and file names (Linux, macOS), test data, protocol tokens and folder names used in code
    r"|^\.{0,2}/|^[^ ]*/[^ ]*/|^(Contents|Library|Google|Mozilla|cache2)/|^</?title>$|://$|\.(dmg|deb|rpm|plist|ttf)$"
    r'|^"\w+"$|^HTTP \d{3}$|^\(Connected\)$|^(aarch64|x86_64|Applications|Resources|ExceptionsList|Firefox|Home|Office VPN)$'
    # code, file masks and type lists, library / app / package names, D-Bus, XDND, DASH and AppleScript tokens
    r"|\(\||&mut |format!|map_err|^\)|^\*\.|^[a-z0-9_.:+@,-]+$|^([a-z0-9]{1,8} ){7,}[a-z0-9]{1,8}$"
    r"|\.(app|AppImage|zip|tar\.gz)$|\.so(\.\d+)*$|\.dll,|^Xdnd|^(org|com)\.|^(BraveSoftware|Yandex)/|^(application|text|video|audio|image)/"
    r"|^(NAME,TYPE|AppleInterfaceStyle|NameHasOwner|ShowItems|httpProxy|Read|DejaVu Sans|Negotiate|Proxy Settings|Brave Browser"
    r"|Microsoft Edge (Beta|Dev)|BaseURL|Representation|Segment(Base|List|Template|URL)|Period|Initialization)$"
    r"|^HTTP(?: [0-9]+)? $|^(AdaptationSet|ETag|Location|bad route|Empty)$"
    r"|^POSIX path|^tell application|\d\d:\d\d:\d\d GMT"
    # yt-dlp's arguments and output, the extension's feed protocol (src/ytdlp.rs, core.rs `feed`)
    r"|^download:ndm |^ERROR: $|^ndm $|^no (at|id)$|^no such download$|^unknown op$|^%\("
    # Set yt-dlp and macOS drops: diagnostics, test data and the registry key (src/ytdlp.rs, mac.rs, win.rs)
    # DASH attributes and PAC functions the parsers look for, the localhost protocol, folder names in code
    r"|^(availabilityStartTime|mediaPresentationDuration|mediaRange|mimeType|minimumUpdatePeriod|presentationTimeOffset"
    r"|sourceURL|startNumber|timeShiftBufferDepth|contentType|dnsResolve|myIpAddress|FindProxyForURL|NoProxyFor|ProxyType"
    r"|NativeMessagingHosts|NDM extension|POST |only CONNECT|no answer|worker panicked|with ndm|open\(|url\(|<body)$"
    r"|^(HTTPEnable|HTTPPort|HTTPProxy|ProxyAutoConfigEnable|ProxyAutoDiscoveryEnable|SOCKSEnable|Proxies|Youtube|NDM crash location |Application crash|Renderer crash)$|^\\n### Local diagnostics"
    r"|^&(gt|lt|nbsp|quot);$|^SHA256 \($"
    r"|^link drops on the main window|^see http://|^Environment$|^Requested format is not available$|^\[(Fixup|Merger\] |VideoConvertor\]|VideoRemuxer\])|^Fixup$|^Destination: $|^\[generic\]$|^Unsupported URL$"
)


def unescape(s):
    """A Rust string literal's text, with newlines as the file's \\n."""
    s = s.replace("\\\\", "\x02").replace('\\"', '"').replace("\\'", "'").replace("\\r\\n", "\\n").replace("\\t", "\t")
    return s.replace("\x02", "\\\\")


# texts the source puts together at run time (View -> Arrange files: tr(&format!("By {n}")))
EXTRA = ["By " + n for n in ["Order Of Addition", "File Name", "Size", "Status", "Time Left", "Transfer Rate", "Last Try Date", "Description", "Save Path",
                             "Referer", "Parent Web Page"]]
# UI words the filters above take for tokens: the OK button, units, the words around number fields
EXTRA += ["OK", "KB", "MB", "GB", "URL", "sec", "min", "seconds", "hours", "every", "times", "sites", "and"]
# the queues NDM makes (core.rs), shown by name; the date format (main.rs `date`, chrono's %-codes)
EXTRA += ["Main download queue", "Synchronization queue", "%b %d %H:%M:%S %Y"]
# a video download's state (ytdlp.rs, shown through the segment info)
EXTRA += ["Joining video and audio..."]
# checkbox labels drawn with dlu::checkbox (Options -> Keyboard)
EXTRA += ["Del"]
# what NDM does after a download (Job::set_post, shown through Info::post)
EXTRA += ["Joining the pieces...", "Getting the audio...", "Moving the file to its folder...", "Processing...", "Processing"]
EXTRA += ["HTTP/1.1...", "HTTP/1.1 hosts"]



def without_tests(src):
    """The source without its tests: #[cfg(test)] items (the tests module, helpers kept among the code) and
    #[test] functions, and without the messages of .expect(...), which only a crash report shows."""
    out, at = [], 0
    for m in re.finditer(r"#\[(?:cfg\(test\)|test)\]", src):
        if m.start() < at:
            continue
        brace, semi = src.find("{", m.end()), src.find(";", m.end())
        if brace < 0 and semi < 0:
            break
        if semi >= 0 and (brace < 0 or semi < brace):
            i = semi  # #[cfg(test)] use ...;
        else:
            depth, i = 0, brace
            while i < len(src):
                if src[i] == "{":
                    depth += 1
                elif src[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
        out.append(src[at:m.start()])
        at = i + 1
    out.append(src[at:])
    return re.sub(r'\.expect\(\s*"(?:[^"\\]|\\.)*"\s*\)', ".expect()", "".join(out))


def strings():
    found = set(EXTRA)
    for path in sorted((Path(ROOT) / "src").rglob("*.rs")):
        if path.name in TEST_ONLY_SOURCES:
            continue
        src = path.read_text(encoding="utf-8")
        src = without_tests(src)  # tests aren't UI
        src = "\n".join(l for l in src.split("\n") if not l.lstrip().startswith("//"))  # nor are comments
        receivers = "|".join(sorted({"d", "d2", *DLU_LOCALS.findall(src)}))
        calls = re.compile(CALLS.replace(r"d2?\.", rf"\b(?:{receivers})\."))
        for m in calls.finditer(src):
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
    return "\n".join(head + [f"{s}={s}" for s in items]) + "\n"


# the slots NDM fills in order (i18n::trf) and the line breaks: a translation keeps them, in the same order
SLOTS = re.compile(r"%(?:I64d|ld|lu|d|s|u|S)|\\n")


def translation_problems(strings):
    """What would show wrong in the translated language files: a line not "English=text", one with other
    placeholders than its English, and UI strings a file doesn't have (they would show in English; a name
    that stays the same in a language is written as "Name=Name")."""
    bad = []
    for path in sorted(glob.glob(os.path.join(ROOT, "Languages", "ndm_*.lng"))):
        if os.path.abspath(path) == os.path.abspath(OUT):
            continue
        name = os.path.basename(path)
        lines = open(path, encoding="utf-8-sig").read().split("\n")[1:]
        have = {l.partition("=")[0] for l in lines if "=" in l and not l.startswith("//")}
        missing = [s for s in strings if s not in have]
        if missing:
            bad.append(f"{name}: {len(missing)} strings not translated: " + "; ".join(repr(s[:40]) for s in missing[:8]) + (" ..." if len(missing) > 8 else ""))
        for n, line in enumerate(lines, 2):
            if not line.strip() or line.startswith("//"):
                continue
            en, sep, t = line.partition("=")
            if not sep:
                bad.append(f"{name}:{n}: not English=translation")
            elif SLOTS.findall(en) != SLOTS.findall(t):
                bad.append(f"{name}:{n}: {en[:50]!r}: {SLOTS.findall(t)} instead of {SLOTS.findall(en)}")
    return bad


if __name__ == "__main__":
    found = strings()
    text = render(found)
    if "--check" in sys.argv:
        same = os.path.exists(OUT) and open(OUT, encoding="utf-8").read() == text
        print("ndm_en.lng is up to date" if same else "ndm_en.lng is out of date: run tools/ndm_lng.py")
        bad = translation_problems(found)
        print("\n".join(bad) if bad else "translations are complete and keep their placeholders")
        sys.exit(0 if same and not bad else 1)
    open(OUT, "w", encoding="utf-8", newline="\n").write(text)
    print(f"{OUT}: {text.count(chr(10)) - 5} strings")
