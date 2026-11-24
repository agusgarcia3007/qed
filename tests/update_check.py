#!/usr/bin/env python3
"""`qed update` against a local stand-in for GitHub releases (releases/latest redirects to
/tag/<v>, assets under /download/<v>/). A copy of qed outside target/ gets replaced.
Usage: update_check.py QED_BIN WORKDIR"""
import hashlib, http.server, io, os, shutil, subprocess, sys, tarfile, threading

qed, wd = (os.path.abspath(a) for a in sys.argv[1:3])
shutil.rmtree(wd, ignore_errors=True)
os.makedirs(f"{wd}/bin")
TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
STATE = {"tag": "v9.9.9", "tamper": False}
fails = []


CACHE = {}


def tarball(version):
    # Built once per version: gzip stamps the time, and the checksum must match the asset.
    if version in CACHE:
        return CACHE[version]
    script = f'#!/bin/sh\necho "qed {version}"\n'.encode()
    b = io.BytesIO()
    with tarfile.open(fileobj=b, mode="w:gz") as t:
        i = tarfile.TarInfo("qed")
        i.size, i.mode = len(script), 0o755
        t.addfile(i, io.BytesIO(script))
    CACHE[version] = b.getvalue()
    return CACHE[version]


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        tag = STATE["tag"]
        tgz = tarball(tag.lstrip("v"))
        sums = "".join(f"{hashlib.sha256(b'x' if STATE['tamper'] else tgz).hexdigest()}  qed-{t}.tar.gz\n" for t in TARGETS)
        if self.path == "/latest":
            self.send_response(302)
            self.send_header("Location", f"/tag/{tag}")
            self.end_headers()
            return
        body = None
        if self.path == f"/tag/{tag}":
            body = b"release page"
        elif self.path == f"/download/{tag}/SHA256SUMS":
            body = sums.encode()
        elif self.path.startswith(f"/download/{tag}/qed-") and self.path.endswith(".tar.gz"):
            body = tgz
        self.send_response(200 if body is not None else 404)
        self.end_headers()
        self.wfile.write(body or b"")


srv = http.server.HTTPServer(("127.0.0.1", 0), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
env = dict(os.environ, QED_RELEASES_URL=f"http://127.0.0.1:{srv.server_port}")
binp = f"{wd}/bin/qed"


def run(*args):
    return subprocess.run([binp, *args], env=env, capture_output=True, text=True)


def check(cond, what, detail=""):
    print(("ok: " if cond else "FAIL: ") + what + ("" if cond else f"\n{detail}"))
    if not cond:
        fails.append(what)


# A checksum that does not match: nothing is replaced.
shutil.copy(qed, binp)
STATE["tamper"] = True
r = run("update")
check(r.returncode == 1 and "checksum" in r.stderr, "a bad checksum is refused", r.stdout + r.stderr)
check(run("--version").stdout.startswith("qed 0."), "and the binary is left alone")
check(not [f for f in os.listdir(f"{wd}/bin") if f.startswith(".qed-update")], "no temp files left behind")

# Already on the latest release.
STATE.update(tag="v0.0.1", tamper=False)
r = run("upgrade")
check(r.returncode == 0 and "up to date" in r.stdout, "up to date when the release is not newer", r.stdout + r.stderr)

# A newer release is downloaded, verified and swapped in.
STATE["tag"] = "v9.9.9"
r = run("update")
check(r.returncode == 0 and "→ 9.9.9" in r.stdout, "update installs the newer release", r.stdout + r.stderr)
check(run("--version").stdout.strip() == "qed 9.9.9", "the binary on disk is the new one")

# A build inside a cargo target/ dir is never replaced.
r = subprocess.run([qed, "update"], env=env, capture_output=True, text=True)
check(r.returncode == 1 and "development build" in r.stderr, "development builds refuse to update", r.stderr)

sys.exit(1 if fails else 0)
