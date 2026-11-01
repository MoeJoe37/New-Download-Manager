"""NDM's own language file: every UI string in NDM's source (src/*.rs) -> Languages/ndm_en.lng.

Reads only NDM's own code, so the result is NDM's to publish. A translation is a
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
CALLS = re.compile(r'(?:trf\(|tr_os\(|tr\(&?|d2?\.(?:label|label_r|text|button|check|check_off|radio|radio_off|group|page_header|readonly)\([^"\n]*?|listview\([^"\n]*?|tabs\([^"\n]*?|\(\s*'
                   # tables of UI text: a tuple's later strings, tuples that start with a value, arrays, menu items
                   r'|(?<=")\s*,\s*|\(\s*(?:(?:true|false|-?\d+(?:\.\d+)?|[A-Za-z_][\w:]*),\s*){1,4}|\[\s*|item\(ui,\s*'
                   # message windows: Dialog::Info { title: "...", text: "..." }; texts picked in if / match arms
                   r'|\b(?:title|text): |\{\s*(?="[^"\n]*"\s*\})|=>\s*)' + LIT)
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
    r"|^POSIX path|^tell application|\d\d:\d\d:\d\d GMT"
    # yt-dlp's arguments and output, the extension's feed protocol (src/ytdlp.rs, core.rs `feed`)
    r"|^download:ndm |^ERROR: $|^ndm $|^no (at|id)$|^no such download$|^unknown op$|^%\("
    # Set yt-dlp and macOS drops: diagnostics, test data and the registry key (src/ytdlp.rs, mac.rs, win.rs)
    r"|^link drops on the main window|^see http://|^Environment$|^Requested format is not available$"
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


def strings():
    found = set(EXTRA)
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
    return "\n".join(head + [f"{s}={s}" for s in items]) + "\n"


# the slots NDM fills in order (i18n::trf) and the line breaks: a translation keeps them, in the same order
SLOTS = re.compile(r"%(?:I64d|ld|lu|d|s|u|S)|\\n")


def translation_problems():
    """Lines of the translated language files that would show wrong: not "English=text", or with other
    placeholders than their English."""
    bad = []
    for path in sorted(glob.glob(os.path.join(ROOT, "Languages", "ndm_*.lng"))):
        if os.path.abspath(path) == os.path.abspath(OUT):
            continue
        name = os.path.basename(path)
        for n, line in enumerate(open(path, encoding="utf-8-sig").read().split("\n")[1:], 2):
            if not line.strip() or line.startswith("//"):
                continue
            en, sep, t = line.partition("=")
            if not sep:
                bad.append(f"{name}:{n}: not English=translation")
            elif SLOTS.findall(en) != SLOTS.findall(t):
                bad.append(f"{name}:{n}: {en[:50]!r}: {SLOTS.findall(t)} instead of {SLOTS.findall(en)}")
    return bad


if __name__ == "__main__":
    text = render(strings())
    if "--check" in sys.argv:
        same = os.path.exists(OUT) and open(OUT, encoding="utf-8").read() == text
        print("ndm_en.lng is up to date" if same else "ndm_en.lng is out of date: run tools/ndm_lng.py")
        bad = translation_problems()
        print("\n".join(bad) if bad else "translations keep their placeholders")
        sys.exit(0 if same and not bad else 1)
    open(OUT, "w", encoding="utf-8", newline="\n").write(text)
    print(f"{OUT}: {text.count(chr(10)) - 5} strings")
