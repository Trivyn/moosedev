"""Confinement for complete study agents and graders, including their children.

Only macOS is supported by this pilot; missing confinement fails closed. The
caller must use private workspace/runtime directories, redirect HOME and TMPDIR,
close inherited descriptors, and exclude credentials not needed by that setup.
This function neither starts processes nor changes their environment.

Network grants allow selected loopback TCP ports and address families. Seatbelt's
network selectors accept only '*' and 'localhost', not literal remote IPs.
Remote HTTPS therefore requires an external domain-filtering loopback proxy;
direct remote grants fail closed. No wildcard HTTPS or DNS fallback is granted.
Explicit unix:///path endpoints may reach only real sockets within runtime.
Daemon listening grants are separate, restricted to explicit loopback TCP ports
or Unix socket paths within runtime; agents and graders receive none by default.
Save the returned command/profile with run evidence. Never broaden a failed
profile automatically. Linux needs separate implementation and runtime validation.
"""

import ipaddress
import os
from pathlib import Path
import platform
import pwd
import stat
import unicodedata
from urllib.parse import unquote, urlsplit


SYSTEM_DIRECTORIES = (
    "/bin", "/sbin", "/usr/bin", "/usr/sbin", "/usr/lib", "/usr/libexec",
    # JavaScriptCore initializes ICU using the system's shared Unicode data.
    # The native OpenCode/Bun executable traps if this data cannot be loaded.
    "/usr/share/icu",
    # ICU's separate timezone database is versioned below this OS-owned root.
    # Apple's system.sb grants the same data subtree for timezone enumeration.
    "/private/var/db/timezone",
    "/System/Library", "/System/Cryptexes/OS",
    "/System/Volumes/Preboot/Cryptexes/OS", "/private/var/db/dyld",
    "/Library/Developer/CommandLineTools",
    "/Applications/Xcode.app/Contents/Developer",
)
SYSTEM_FILES = (
    "/etc/localtime", "/private/etc/ssl/openssl.cnf",
    "/private/etc/ssl/cert.pem",
)
REPOSITORY = Path(__file__).resolve().parents[2]


def sandbox_literal(value):
    """Quote an SBPL string; reject controls rather than interpreting escapes."""
    value = str(value)
    if any(unicodedata.category(char).startswith("C") for char in value):
        raise ValueError("sandbox value contains control characters")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _checked_path(value, *, directory=False):
    path = Path(value)
    sandbox_literal(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError(f"sandbox paths must be absolute without '..': {path}")
    for ancestor in (*reversed(path.parents), path):
        mode = ancestor.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise ValueError(f"sandbox paths must not traverse symlinks: {ancestor}")
        if ancestor != path and not stat.S_ISDIR(mode):
            raise ValueError(f"sandbox ancestor must be a directory: {ancestor}")
    mode = path.lstat().st_mode
    if not (stat.S_ISDIR(mode) or (not directory and stat.S_ISREG(mode))):
        raise ValueError(f"sandbox grant must be a real {'directory' if directory else 'file or directory'}: {path}")
    return path


def _reject_broad(path):
    actual_home = Path(pwd.getpwuid(os.getuid()).pw_dir)
    broad = {
        Path("/"), Path("/Users"), Path("/home"), Path("/private"),
        Path("/private/tmp"), Path("/private/var"), Path("/var"), Path("/tmp"),
        Path("/opt"), Path("/opt/homebrew"), Path("/usr"), Path("/usr/local"),
        Path("/Library"), Path("/System"), Path("/Applications"),
        actual_home, actual_home.parent, REPOSITORY,
    }
    if path in broad or REPOSITORY.is_relative_to(path):
        raise ValueError(f"refusing broad sandbox grant: {path}")


def _readable_asset(value, workspace, runtime):
    path = _checked_path(value)
    _reject_broad(path)
    if (workspace.is_relative_to(path) and path != workspace
            or runtime.is_relative_to(path) and path != runtime):
        raise ValueError(f"readable grant exposes a parent of isolated storage: {path}")
    # A checkout's target assets may be explicitly exposed; granting its source
    # would defeat the study even when that source is only a narrow subdirectory.
    for ancestor in (path, *path.parents):
        if (ancestor / ".git").exists() and ancestor not in (workspace, runtime):
            if not path.is_relative_to(ancestor / "target"):
                raise ValueError(f"readable asset exposes a source checkout: {path}")
    return path


def _network_targets(endpoints, runtime=None, *, listening=False):
    targets = set()
    unix_sockets = set()
    for endpoint in endpoints:
        sandbox_literal(endpoint)
        parsed = urlsplit(endpoint)
        if parsed.scheme == "unix":
            path = Path(unquote(parsed.path))
            sandbox_literal(path)
            if (runtime is None or parsed.netloc or parsed.query or parsed.fragment
                    or not path.is_absolute() or ".." in path.parts
                    or not path.is_relative_to(runtime)):
                raise ValueError("Unix socket endpoints must be absolute paths within runtime")
            _checked_path(path.parent, directory=True)
            try:
                mode = path.lstat().st_mode
            except FileNotFoundError:
                if not listening:
                    raise ValueError("outbound Unix endpoint must already exist") from None
            else:
                if not stat.S_ISSOCK(mode):
                    raise ValueError("Unix endpoint must be a real socket")
            unix_sockets.add(str(path))
            continue
        if (parsed.scheme not in ("http", "https") or not parsed.hostname
                or parsed.username is not None or parsed.password is not None
                or parsed.query or parsed.fragment):
            raise ValueError("network endpoint must be an explicit HTTP(S) URL without credentials")
        try:
            port = parsed.port
        except ValueError as error:
            raise ValueError("invalid network endpoint port") from error
        host = parsed.hostname
        try:
            address = ipaddress.ip_address(host)
        except ValueError:
            address = None
        if host == "localhost":
            addresses = [ipaddress.ip_address("127.0.0.1"), ipaddress.ip_address("::1")]
        elif address is not None:
            addresses = [address]
        else:
            raise ValueError("remote endpoints require a domain-filtering loopback proxy")
        for address in addresses:
            if str(address) not in {"127.0.0.1", "::1"}:
                raise ValueError("only standard loopback endpoints are supported; remote access requires a loopback proxy")
            if port is None:
                raise ValueError("loopback endpoints require an explicit port")
            if port == 0:
                raise ValueError("network endpoint port must be positive")
            targets.add((str(address), port))
    return sorted(targets), False, sorted(unix_sockets)


def _tcp_filter(address, port, direction):
    address = ipaddress.ip_address(address)
    if (str(address) not in {"127.0.0.1", "::1"} or not isinstance(port, int)
            or isinstance(port, bool) or not 1 <= port <= 65535):
        raise ValueError("TCP grants require standard loopback addresses and explicit ports")
    return f'({direction} tcp{address.version} "localhost:{port}")'


def sandbox_profile(*, readable_directories, readable_files, writable_directories,
                    network_targets=(), system_dns=False, unix_sockets=(),
                    listening_targets=(), listening_sockets=()):
    """Pure SBPL rendering of already validated paths and TCP address/port pairs."""
    if system_dns:
        raise ValueError("direct DNS is unsupported; hosted clients require a loopback proxy")
    rules = [
        "(version 1)", "(deny default)", "(allow file-read-metadata)",
        "(allow process-fork)", "(allow sysctl-read)",
        "(allow signal (target same-sandbox))",
    ]
    for path in sorted(set(map(str, readable_directories))):
        rules.append(f"(allow file-read* process-exec (subpath {sandbox_literal(path)}))")
    for path in sorted(set(map(str, readable_files))):
        rules.append(f"(allow file-read* process-exec (literal {sandbox_literal(path)}))")
    # libignition opens the root directory during dyld bootstrap. A literal rule
    # grants no descendant contents. Device grants do not expose /dev/fd.
    for path in ("/", "/dev/null", "/dev/random", "/dev/urandom"):
        rules.append(f"(allow file-read* (literal {sandbox_literal(path)}))")
    for path in sorted(set(map(str, writable_directories))):
        literal = sandbox_literal(path)
        rules.extend((f"(allow file-write* (subpath {literal}))",
                      f"(deny file-write-unlink (literal {literal}))"))
    rules.extend(("(deny file-write-flags)", '(allow file-write-data (literal "/dev/null"))'))
    for address, port in sorted(set(network_targets)):
        rules.append(f"(allow network-outbound {_tcp_filter(address, port, 'remote')})")
    for path in sorted(set(unix_sockets)):
        rules.append(f"(allow network-outbound (literal {sandbox_literal(path)}))")
    for address, port in sorted(set(listening_targets)):
        rules.append(f"(allow network-bind network-inbound {_tcp_filter(address, port, 'local')})")
    for path in sorted(set(listening_sockets)):
        rules.append(f"(allow network-bind network-inbound (literal {sandbox_literal(path)}))")
    return "\n".join(rules) + "\n"


def sandbox_command(command: list[str], *, workspace: Path, runtime: Path,
                    readable_paths=None, network_endpoints=None,
                    listening_endpoints=None) -> list[str]:
    """Return a sandbox-exec argv; no shell interpolation or unconfined fallback."""
    if platform.system() != "Darwin":
        raise RuntimeError("study isolation currently requires validated macOS Seatbelt; Linux is unsupported")
    executable = Path("/usr/bin/sandbox-exec")
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise RuntimeError("macOS sandbox-exec is required; refusing unconfined execution")
    if not command or any(not isinstance(item, str) or "\0" in item for item in command):
        raise ValueError("command must be a nonempty argv of strings without NUL")
    if not Path(command[0]).is_absolute():
        raise ValueError("sandbox command executable must be explicit and absolute")
    workspace = _checked_path(workspace, directory=True)
    runtime = _checked_path(runtime, directory=True)
    for path in (workspace, runtime):
        _reject_broad(path)
    if workspace.is_relative_to(runtime) or runtime.is_relative_to(workspace):
        raise ValueError("workspace and runtime must be separate nonoverlapping directories")
    assets = [_readable_asset(path, workspace, runtime) for path in (readable_paths or ())]
    directories = [Path(path).resolve() for path in SYSTEM_DIRECTORIES if Path(path).is_dir()]
    files = [Path(path).resolve() for path in SYSTEM_FILES if Path(path).is_file()]
    directories.extend((workspace, runtime))
    directories.extend(path for path in assets if path.is_dir())
    files.extend(path for path in assets if path.is_file())
    targets, needs_dns, unix_sockets = _network_targets(network_endpoints or (), runtime)
    listening_targets, _, listening_sockets = _network_targets(
        listening_endpoints or (), runtime, listening=True)
    profile = sandbox_profile(readable_directories=directories, readable_files=files,
                              writable_directories=[workspace, runtime],
                              network_targets=targets, system_dns=needs_dns,
                              unix_sockets=unix_sockets,
                              listening_targets=listening_targets,
                              listening_sockets=listening_sockets)
    return [str(executable), "-p", profile, *command]
