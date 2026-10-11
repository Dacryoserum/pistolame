"""Package the native player with licenses and explicit platform limitations."""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile
import tomllib
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=["windows-x64", "macos-universal", "linux-x64"], required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if os.environ.get("GITHUB_REF_TYPE") == "tag" and os.environ.get("GITHUB_REF_NAME") != f"v{version}":
        raise SystemExit("Release tag must equal the Cargo workspace version")
    name = f"Pistolame-{version}-{args.platform}"
    folder = root / "target" / "packaging" / name
    folder.mkdir(parents=True, exist_ok=False)
    for source, destination in [
        ("LICENSE", "LICENSE"), ("README.md", "README.md"),
        ("RELEASE_NOTES.md", "RELEASE_NOTES.md"), ("CHANGELOG.md", "CHANGELOG.md"),
        ("crates/pistolame-app/assets/fonts/OFL.txt", "Inter-OFL.txt"),
        ("crates/pistolame-app/assets/icons/LICENSE", "Phosphor-LICENSE"),
    ]:
        shutil.copyfile(root / source, folder / destination)
    binary = root / "target" / "release" / ("pistolame-app.exe" if args.platform == "windows-x64" else "pistolame-app")
    if args.platform == "macos-universal":
        app = folder / "Pistolame.app"
        executable = app / "Contents" / "MacOS" / "Pistolame"
        executable.parent.mkdir(parents=True)
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        info = {
            "CFBundleName": "Pistolame", "CFBundleDisplayName": "Pistolame",
            "CFBundleExecutable": "Pistolame", "CFBundleIdentifier": "com.dacryoserum.pistolame",
            "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version.split("-")[0],
            "CFBundleVersion": version.split("-")[0], "LSMinimumSystemVersion": "11.0",
            "NSHighResolutionCapable": True,
        }
        (app / "Contents" / "Info.plist").write_bytes(plistlib.dumps(info))
        subprocess.run(["plutil", "-lint", str(app / "Contents" / "Info.plist")], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
        subprocess.run(["lipo", "-verify_arch", "arm64", "x86_64", str(executable)], check=True)
    else:
        executable = folder / ("Pistolame.exe" if args.platform == "windows-x64" else "Pistolame")
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
    dist = root / "target" / "dist"
    dist.mkdir(exist_ok=True)
    if args.platform == "linux-x64":
        archive = dist / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(folder, arcname=name)
        with tarfile.open(archive) as output:
            assert output.getmember(f"{name}/Pistolame").mode & 0o111
    else:
        archive = dist / f"{name}.zip"
        if args.platform == "macos-universal":
            subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(folder), str(archive)], check=True)
        else:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
                for item in sorted(folder.rglob("*")):
                    if item.is_file():
                        output.write(item, item.relative_to(folder.parent))
        with zipfile.ZipFile(archive) as output:
            assert output.testzip() is None
            assert f"{name}/LICENSE" in output.namelist()
    digest = hashlib.file_digest(archive.open("rb"), "sha256").hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n")
    print(f"Verified {archive.name}: {archive.stat().st_size} bytes, SHA256 {digest}")


if __name__ == "__main__":
    main()
