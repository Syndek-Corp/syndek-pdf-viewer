"""Turn a PdfCraft checkout into DocuView, in place. The DocuView build runs this before packaging.

Why a build step instead of editing the source: the rest of this repository stays identical to
upstream (storytold/pdfcraft), so "Sync fork" never conflicts. Everything Syndek-specific lives
in branding/ and .github/workflows/docuview.yml, which upstream never touches.

Every replacement listed below must match. If upstream rewords or moves something, this script
stops the build and names what it couldn't find, instead of shipping a half-branded app; fix the
entry here and rebuild.

    python branding/apply.py          # modifies the working tree; don't commit the result
    git checkout -- . ':!branding'    # undo locally
"""

from __future__ import annotations

import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = Path(__file__).resolve().parent / "assets"

APP_NAME = "DocuView"
COMPANY = "Syndek Corp"
REPO = "Syndek-Corp/syndek-pdf-viewer"
REPO_URL = f"https://github.com/{REPO}"
# Where the in-app "Feedback" button goes. Testers without a GitHub account can't file issues;
# switch this to e.g. "mailto:it@yourcompany.com" if that suits them better.
FEEDBACK_URL = f"{REPO_URL}/issues/new"
RELEASES_URL = f"{REPO_URL}/releases"
BUNDLE_ID = "com.syndek.docuview"

# Fixed for the life of the product: Windows uses these to recognise DocuView on upgrade and
# uninstall. New ones (not PdfCraft's) so DocuView and PdfCraft never replace each other.
UPGRADE_CODE = "E0447B82-8144-4EA3-8043-75B6B34D6E22"
COMPONENT_GUIDS = {
    "E724C1C6-091F-4E21-A685-80D089FDBE21": "8E1F3441-9B7B-46CA-A749-7A7D96B25BFC",  # app
    "DC028192-C233-4F99-9A77-ED458C60DF26": "EA5B40C4-4AB3-4EBD-910C-54C79F6AD524",  # Start Menu shortcut
    "F6948627-32AB-4D78-8865-0FDF417CE442": "0848DA53-8DA1-479A-A875-A807ABBD2797",  # desktop shortcut
    "439F084F-657D-4AC2-A4CC-F099C518D71F": "3C29367F-5330-4D07-A510-8A460D798B22",  # PDF context menu
    "1B783BF3-12DF-4588-AF80-E2CEA8BC1E5A": "30CA1F29-0CE3-4235-B2E1-E3625EF186C0",  # image context menu (0.9.0)
}
GUID_RE = re.compile(r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}")

# 1. The display name, as a whole word ("PdfCraft", not "PdfCraftApp" or "pdfcraft"), in the
#    files people see the name from: window, menus, dialogs, installer, macOS bundle.
NAME_SCOPE = [
    "apps/pdfcraft/build.rs",
    "apps/pdfcraft/src/**/*.rs",
    "crates/ui-egui/src/**/*.rs",
    "crates/ui-egui/src/i18n/*.tsv",
    "crates/engine/src/commands.rs",
    "crates/engine/src/links.rs",
    "packaging/windows/*.wxs",
    "packaging/windows/*.ps1",
    "packaging/macos/package.sh",
    "packaging/macos/Info.plist.in",
]
NAME_RE = re.compile(r"\bPdfCraft\b")

# 2. Exact edits, applied after step 1 (so "PdfCraft" in an old string already reads "DocuView").
#    (file, old, new, expected count or None for "at least one")
EDITS: list[tuple[str, str, str, int | None]] = [
    # Help menu, About dialog and home screen links (ArtCraft/Discord -> Syndek).
    ("crates/engine/src/links.rs", '"https://discord.gg/artcraft"', f'"{FEEDBACK_URL}"', 1),
    ("crates/engine/src/links.rs", '"https://getartcraft.com"', f'"{RELEASES_URL}"', 1),
    ("crates/engine/src/links.rs", '"https://getartcraft.com/apps/pdfcraft"', f'"{REPO_URL}"', 1),
    ("crates/engine/src/links.rs", '"https://github.com/storytold/pdfcraft"', f'"{REPO_URL}"', 1),
    ("crates/engine/src/links.rs", '"Join the ArtCraft Discord"', '"Send feedback"', 1),
    ("crates/engine/src/links.rs", '"ArtCraft website"', '"Download the latest version"', 1),
    ("crates/engine/src/commands.rs", '"Join the ArtCraft Discord"', '"Send feedback"', 1),
    ("crates/engine/src/commands.rs", '"ArtCraft website"', '"Download the latest version"', 1),
    ("crates/ui-egui/src/widgets.rs", 'tl!("Join our Discord")', 'tl!("Send feedback")', 1),
    ("crates/ui-egui/src/chrome.rs", '"Discord"', '"Feedback"', 2),
    ("crates/ui-egui/src/home.rs", 'tl!("Join the ArtCraft community")', f'tl!("Help us improve {APP_NAME}")', 1),
    (
        "crates/ui-egui/src/home.rs",
        'tl!("Get help, share feedback and follow development on Discord.")',
        'tl!("Report a problem or suggest an improvement.")',
        1,
    ),
    # About dialog: credit the upstream project; "Part of [ArtCraft logo]" -> "Made for Syndek Corp".
    (
        "crates/ui-egui/src/dialogs.rs",
        'tl!("A clean-room, open-source PDF application written in Rust. MIT OR Apache-2.0.")',
        '"Based on the open-source PdfCraft project (github.com/storytold/pdfcraft). MIT OR Apache-2.0."',
        1,
    ),
    ("crates/ui-egui/src/dialogs.rs", 'tl!("Part of")', '"Made for"', 1),
    (
        "crates/ui-egui/src/dialogs.rs",
        "widgets::artcraft_logo(ui, 16.0);",
        f'ui.label(egui::RichText::new("{COMPANY}").font(theme::semibold(14.0)));',
        1,
    ),
    # Help > Check for updates asks this repository's releases, not upstream's.
    ("crates/ui-egui/src/updates.rs", "https://github.com/storytold/pdfcraft/releases", RELEASES_URL, 1),
    ("apps/pdfcraft/src/updates.rs", "https://api.github.com/repos/storytold/pdfcraft/releases/latest", f"https://api.github.com/repos/{REPO}/releases/latest", 1),
    # Windows executable details (Properties > Details, Task Manager).
    ("apps/pdfcraft/build.rs", '"DocuView PDF workbench"', f'"{APP_NAME} PDF viewer"', 1),
    (
        "apps/pdfcraft/build.rs",
        '"Copyright (c) the DocuView contributors. MIT OR Apache-2.0."',
        f'"Copyright (c) {COMPANY} and the PdfCraft contributors. MIT OR Apache-2.0."',
        1,
    ),
    # Installer: publisher, support links, and DocuView's own installer identity.
    ("packaging/windows/pdfcraft.wxs", 'Manufacturer="Learning Machines LLC"', f'Manufacturer="{COMPANY}"', 2),
    ("packaging/windows/test-msi.ps1", "'Learning Machines LLC'", f"'{COMPANY}'", 1),
    # The UpgradeCode, and (since 0.9.0) the <Upgrade> table that detects earlier installs of it.
    ("packaging/windows/pdfcraft.wxs", "EA00C367-7270-4C50-9D8B-A7A5C88AD27F", UPGRADE_CODE, None),
    *[("packaging/windows/pdfcraft.wxs", old, new, 1) for old, new in COMPONENT_GUIDS.items()],
    ("packaging/windows/pdfcraft.wxs", '"https://getartcraft.com/apps/pdfcraft"', f'"{RELEASES_URL}"', 2),
    ("packaging/windows/pdfcraft.wxs", '"https://discord.gg/artcraft"', f'"{FEEDBACK_URL}"', 1),
    ("packaging/windows/pdfcraft.wxs", '"DocuView PDF workbench"', f'"{APP_NAME} PDF viewer"', None),
    ("packaging/windows/pdfcraft.wxs", '"Open-source PDF workbench"', '"PDF viewer and editor"', 1),
    # macOS About panel (since 0.9.0).
    (
        "packaging/macos/Credits.rtf",
        "A clean-room, open-source PDF application written in Rust.",
        "Based on the open-source PdfCraft project (github.com/storytold/pdfcraft).",
        1,
    ),
    (
        "packaging/macos/Credits.rtf",
        '{\\field{\\*\\fldinst{HYPERLINK "https://getartcraft.com"}}{\\fldrslt getartcraft.com}}',
        f"Made for {COMPANY}",
        1,
    ),
    (
        "packaging/macos/Credits.rtf",
        '{\\field{\\*\\fldinst{HYPERLINK "https://discord.gg/artcraft"}}{\\fldrslt discord.gg/artcraft}}',
        '{\\field{\\*\\fldinst{HYPERLINK "' + FEEDBACK_URL + '"}}{\\fldrslt Send feedback}}',
        1,
    ),
    # macOS bundle identity.
    ("packaging/macos/Info.plist.in", "ai.storyteller.pdfcraft", BUNDLE_ID, None),
    ("packaging/macos/Info.plist.in", "Copyright © the DocuView authors.", f"Copyright © {COMPANY} and the PdfCraft authors.", 1),
]

# 3. Files replaced outright: the app icon, and the ArtCraft brand marks (trademarks that forks
#    must remove, docs/brand/LICENSE-brand.txt) swapped for the DocuView placeholder.
COPIES = [
    ("app.ico", "assets/app-icon/pdfcraft.ico"),
    ("app.icns", "assets/app-icon/pdfcraft.icns"),
    ("app-1024.png", "assets/app-icon/pdfcraft-1024.png"),
    ("app.svg", "assets/app-icon/pdfcraft.svg"),
    ("app.svg", "assets/app-icon/pdfcraft-small.svg"),
    ("app.svg", "assets/app-icon/hicolor/scalable/apps/ai.storyteller.pdfcraft.svg"),
    *[(f"hicolor/{n}x{n}/app.png", f"assets/app-icon/hicolor/{n}x{n}/apps/ai.storyteller.pdfcraft.png") for n in (16, 24, 32, 48, 64, 128, 256, 512)],
    ("app.svg", "docs/brand/artcraft-mark.svg"),
    ("app.svg", "docs/brand/artcraft-mark-black.svg"),
    ("wordmark.svg", "docs/brand/artcraft-logo.svg"),
    ("wordmark-white.svg", "docs/brand/artcraft-logo-white.svg"),
]


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def write(path: Path, text: str) -> None:
    # newline="" keeps each file's own line endings.
    with path.open("w", encoding="utf-8", newline="") as f:
        f.write(text)


def main() -> int:
    problems: list[str] = []

    renamed = 0
    for pattern in NAME_SCOPE:
        files = sorted(ROOT.glob(pattern))
        if not files:
            problems.append(f"no files match {pattern}")
        for path in files:
            text = read(path)
            new, n = NAME_RE.subn(APP_NAME, text)
            if n:
                write(path, new)
                renamed += n
    print(f"renamed PdfCraft -> {APP_NAME} in {renamed} places")

    for rel, old, new, expected in EDITS:
        path = ROOT / rel
        if not path.is_file():
            problems.append(f"{rel}: file is gone")
            continue
        text = read(path)
        found = text.count(old)
        if found == 0 or (expected is not None and found != expected):
            want = "at least 1" if expected is None else str(expected)
            problems.append(f"{rel}: expected {want} of {old!r}, found {found}")
            continue
        write(path, text.replace(old, new))
    print(f"applied {len(EDITS)} edits")

    # Every installer GUID must now be one of DocuView's. A GUID upstream adds later (a new
    # component) would otherwise be shared with PdfCraft installs; give it its own entry above.
    ours = {UPGRADE_CODE, *COMPONENT_GUIDS.values()}
    for path in sorted((ROOT / "packaging/windows").glob("*.wxs")):
        for guid in sorted(set(GUID_RE.findall(read(path))) - ours):
            problems.append(f"{path.relative_to(ROOT).as_posix()}: GUID {guid} is upstream's; add a DocuView one to COMPONENT_GUIDS")

    for src, dst in COPIES:
        target = ROOT / dst
        if not target.exists():
            problems.append(f"{dst}: file to replace is gone")
            continue
        shutil.copyfile(ASSETS / src, target)
    print(f"replaced {len(COPIES)} icon and logo files")

    if problems:
        print("\nBranding failed; upstream changed something this script relies on:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    print(f"{APP_NAME} branding applied")
    return 0


if __name__ == "__main__":
    sys.exit(main())
