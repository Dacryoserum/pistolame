"""Signs the release archives and writes latest.json, the manifest the app's updater reads.

Usage (release workflow, draft job):
    UPDATE_SIGNING_KEY=<ed25519 PEM> RELEASE_TAG=v0.1.0-preview.4 REPOSITORY=owner/name \\
        python scripts/write_update_manifest.py dist

Each archive gets its SHA-256 and an ed25519 signature made with OpenSSL 3. The private key is
written to a temporary file readable only by this user and deleted at the end.
"""
import base64
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

# Archive names from package_release.py: Pistolame-<version>-<platform>.<zip|tar.gz>
ARCHIVE_NAME = re.compile(r"^Pistolame-(?P<version>.+)-(?P<platform>macos-universal|windows-x64|linux-x64)\.(zip|tar\.gz)$")


def release_notes(root: pathlib.Path) -> str:
    """The user-facing part of RELEASE_NOTES.md (before the generated changelog)."""
    text = (root / "RELEASE_NOTES.md").read_text(encoding="utf-8")
    return text.split("\n# Changelog", 1)[0].strip()


def sign(archive: pathlib.Path, key_file: pathlib.Path) -> str:
    signature = subprocess.run(
        ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", str(key_file), "-in", str(archive)],
        check=True,
        capture_output=True,
    ).stdout
    if len(signature) != 64:
        raise SystemExit(f"unexpected ed25519 signature length for {archive.name}")
    return base64.b64encode(signature).decode()


def main() -> None:
    dist = pathlib.Path(sys.argv[1])
    root = pathlib.Path(__file__).resolve().parent.parent
    key = os.environ.get("UPDATE_SIGNING_KEY", "")
    tag = os.environ["RELEASE_TAG"]
    repository = os.environ["REPOSITORY"]
    if "PRIVATE KEY" not in key:
        raise SystemExit("UPDATE_SIGNING_KEY secret is missing: updates cannot be signed")
    version = tag.removeprefix("v")
    assets = {}
    with tempfile.TemporaryDirectory() as folder:
        key_file = pathlib.Path(folder) / "key.pem"
        key_file.touch(mode=0o600)
        key_file.write_text(key if key.endswith("\n") else key + "\n")
        for archive in sorted(dist.iterdir()):
            match = ARCHIVE_NAME.match(archive.name)
            if not match:
                continue
            if match["version"] != version:
                raise SystemExit(f"{archive.name} does not match tag {tag}")
            assets[match["platform"]] = {
                "url": f"https://github.com/{repository}/releases/download/{tag}/{archive.name}",
                "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
                "signature": sign(archive, key_file),
                "size": archive.stat().st_size,
            }
    if len(assets) != 3:
        raise SystemExit(f"expected 3 platform archives, found {sorted(assets)}")
    manifest = {"version": version, "notes": release_notes(root), "assets": assets}
    (dist / "latest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(f"latest.json written for {version}: {', '.join(sorted(assets))}")


if __name__ == "__main__":
    main()
