# DocuView (Syndek Corp build of PdfCraft)

This repository is a fork of [storytold/pdfcraft](https://github.com/storytold/pdfcraft). Everything
Syndek-specific lives in two places:

- `branding/` holds the name, links, icons and installer identity, applied by `apply.py` at build time
- `.github/workflows/docuview.yml` is the build that produces the installers

Nothing else is edited, so updates from upstream never conflict.

## Get upstream's latest fixes
On GitHub, open the repo's main page and click **Sync fork → Update branch**. That's it.

## Build a new version for testers
1. **Actions → DocuView Build → Run workflow** (about 30–60 minutes).
2. When it finishes, the **Releases** page has the new `.msi` (Windows) and `.dmg` (Mac).
3. Testers download and run the new MSI over the old one. In the app, **Help → Check for updates**
   also points to these releases.

If the build fails at **Apply DocuView branding**, upstream reworded or moved something the script
relies on. The log names the exact file and text; update that entry in `apply.py`.

## Change the name, links or logo
- Name, company, feedback link: the settings at the top of `apply.py`.
- Logo: replace the files in `branding/assets/` (same names and sizes), or edit and rerun
  `python branding/make_icons.py`.
- Never change `UPGRADE_CODE` or `COMPONENT_GUIDS` after release. Windows uses them to upgrade and
  uninstall DocuView.

## Signing
Windows signing turns on automatically once the six `AZURE_*` secrets are set in the `release`
environment (see issue #1). Until then the build ships unsigned and Windows shows an
"unknown publisher" prompt.

## Cutting off from upstream (later)
1. Run `python branding/apply.py` once and commit the result, so the branding becomes permanent.
2. Create a new **private** repo in Syndek-Corp and push this one to it (`git push --mirror`).
3. Archive this public fork.
4. Fonts: the build checks out `storytold/craft-fonts` at a pinned commit. Copy that repository into
   Syndek-Corp too and point the workflow at the copy.

## License
PdfCraft is MIT OR Apache-2.0. Keep `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE` (the installer
ships them). The ArtCraft name and logos are trademarks that forks must not use; `apply.py` replaces them.
