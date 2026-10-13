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
    # A restored CI cache of `target/` can hold the previous run's folder: start clean.
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(parents=True)
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
            "NSHighResolutionCapable": True, "CFBundleIconFile": "Pistolame",
        }
        resources = app / "Contents" / "Resources"
        resources.mkdir()
        shutil.copyfile(root / "crates/pistolame-app/assets/app-icon/Pistolame.icns", resources / "Pistolame.icns")
        (app / "Contents" / "Info.plist").write_bytes(plistlib.dumps(info))
        subprocess.run(["plutil", "-lint", str(app / "Contents" / "Info.plist")], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
        subprocess.run(["lipo", str(executable), "-verify_arch", "arm64", "x86_64"], check=True)
    else:
        executable = folder / ("Pistolame.exe" if args.platform == "windows-x64" else "Pistolame")
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        if args.platform == "linux-x64":
            # Icon and desktop entry for menus and docks (freedesktop.org conventions).
            shutil.copyfile(root / "crates/pistolame-app/assets/app-icon/pistolame-256.png", folder / "pistolame.png")
            (folder / "pistolame.desktop").write_text(
                "[Desktop Entry]\nType=Application\nName=Pistolame\n"
                "Comment=Lossless audio player for audiophiles\nExec=Pistolame\n"
                "Icon=pistolame\nCategories=AudioVideo;Audio;Player;\nTerminal=false\n"
            )
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
    write_checksum(archive)
    if args.platform == "windows-x64":
        write_checksum(build_windows_installer(root, folder, version, dist))


def write_checksum(path):
    digest = hashlib.file_digest(path.open("rb"), "sha256").hexdigest()
    path.with_name(path.name + ".sha256").write_text(f"{digest}  {path.name}\n")
    print(f"Verified {path.name}: {path.stat().st_size} bytes, SHA256 {digest}")


def build_windows_installer(root, folder, version, dist):
    """Pistolame-<version>-windows-x64-setup.exe from the packaged folder (Inno Setup 6)."""
    compiler = os.environ.get("INNO_SETUP_COMPILER", r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe")
    if not Path(compiler).is_file():
        raise SystemExit(f"Inno Setup compiler not found: {compiler}")
    subprocess.run(
        [compiler, f"/DAppVersion={version}", f"/DSourceDir={folder}", f"/DOutputDir={dist}",
         str(root / "installer" / "pistolame.iss")],
        check=True,
    )
    installer = dist / f"Pistolame-{version}-windows-x64-setup.exe"
    if not installer.is_file():
        raise SystemExit(f"installer missing: {installer}")
    return installer


if __name__ == "__main__":
    main()
