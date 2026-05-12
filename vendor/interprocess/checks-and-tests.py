#!/usr/bin/env python3
import subprocess
import sys
from os import environ
from pathlib import Path

### begin SGR helpers ###
CSI = '\x1b['
def end_code(c):
    if  1 <= c <=  2: return 22
    if  5 <= c <=  6: return 25
    if  3 <= c <=  4 or   7 <= c <=   9: return c + 20
    if 30 <= c <= 37 or  90 <= c <=  97: return 39
    if 40 <= c <= 47 or 100 <= c <= 107: return 49

def color(s, c):
    c = int(c)
    return f'{CSI}{c}m{s}{CSI}{end_code(c)}m'
def bold  (s): return color(s, 1)
def italic(s): return color(s, 3)
def red   (s): return color(s, 31)
def green (s): return color(s, 32)
def yellow(s): return color(s, 33)
def blue  (s): return color(s, 34)
ERROR = bold(red('error') + ':')
### end SGR helpers ###

### begin I/O helpers ###
builtin_print = print
prev_r_len = None
in_erasing_line = False
indent = 0
def _erase(): builtin_print('\r', ' ' * prev_r_len, '\r', sep='', end='', flush=False, file=sys.stderr)
def print(*s, erase=True, sep=' ', end='\n'):
    global prev_r_len, in_erasing_line, indent
    if indent < 0: indent = 0
    s = '    ' * indent + sep.join(str(c) for c in s)
    slen = len(s)
    if prev_r_len is not None:
        #if erase and in_erasing_line: _erase()
        #else: s = '\n' + s
        _erase()
    if erase and in_erasing_line:
        prev_r_len = slen
        end = ''
    else:
        prev_r_len = None
    builtin_print(s, sep='', end=end, flush=True, file=sys.stderr)

class ErasingLine:
    def __enter__(self):
        global in_erasing_line
        in_erasing_line = True
    def __exit__(self, exc_type, exc_value, traceback):
        global prev_r_len, in_erasing_line
        if prev_r_len is not None: _erase()
        in_erasing_line = False
        prev_r_len = None

class Indent:
    def __enter__(self):
        global indent
        indent += 1
    def __exit__(self, exc_type, exc_value, traceback):
        global indent
        indent -= 1
### end I/O helpers ###

WARNING_DISPOSITION = ['-A', 'unknown-lints', '-D', 'warnings']

CARGO = environ.get('CARGO', 'cargo')

def run(a, *, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, **kwargs):
    if stderr == subprocess.STDOUT and stdout is None: stderr = None
    r = subprocess.run(a, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, text=text, **kwargs)
    if r.returncode != 0:
        out = r.stdout if stderr == subprocess.STDOUT else r.stderr
        if out is not None: print(out, end='', sep='', erase=False)
    return r

def read_msrv():
    with open(MANIFEST_DIR / Path('Cargo.toml')) as f:
        in_package = False
        for l in (l for l in map(str.strip, f) if len(l) >= 2):
            if l[0] == '[' and l[-1] == ']':
                in_package = l == '[package]'
                continue
            if not in_package: continue
            l = [c for c in map(str.strip, l.split('=')) if len(c) != 0]
            if len(l) < 2: continue
            if l[0] == 'rust-version':
                v = l[1]
                if v[0] != '"' or v[-1] != '"': raise RuntimeError('rust-version is not a string')
                v = v[1:-1]
                return v
        raise RuntimeError('no rust-version key found')

def check_rustup():
    r = run([CARGO, f'+{MSRV}', '--version'], stdout=subprocess.DEVNULL)
    return r.returncode == 0

def cargo_invoc(vers):
    if vers is None: return [CARGO]
    vers = vers.strip()
    if len(vers) == 0: return [CARGO]
    return [CARGO, f'+{vers}']

def get_host_target():
    args = cargo_invoc(MSRV if HAS_RUSTUP else None) + ['rustc', '--quiet', '--lib', '--', '--version', '--verbose']
    r = run(args, stderr=subprocess.PIPE)
    if r.returncode != 0: raise RuntimeError(f'could not determine the host target: {r.stderr}')
    for l in map(str.strip, r.stdout.split('\n')):
        if l.startswith('host: '): return l[6:]
    raise RuntimeError('output of `cargo rustc -- --version --verbose` did not contain a `host: ` line')

def check_clippy():
    r = run(cargo_invoc(MSRV if HAS_RUSTUP else None) + ['clippy', '--version'], stdout=subprocess.DEVNULL)
    return r.returncode == 0

def cargo(vers, status, subcommand, args=[], *, frozen=True, capture=True):
    if isinstance(args, str): args = [args]
    args = cargo_invoc(vers)               \
        + [subcommand]                     \
        + ['--quiet', '--color', 'always'] \
        + (['--frozen'] if frozen else []) \
        + list(args)
    print(italic(f'{status}…'))
    r = run(args, stdout=subprocess.PIPE if capture else None)
    if r.returncode != 0: sys.exit(r.returncode)

SAVED_ENC_RUSTFLAGS, SAVED_ENC_RUSTDOCFLAGS = [environ.get(f'CARGO_ENCODED_RUST{v}FLAGS') for v in ('', 'DOC')]
def _set_warn_var(var, old_val, *, allow_unknown_lints):
    pfx = old_val + '\x1f' if old_val is not None else ''
    new_flags = '-Dwarnings'
    if allow_unknown_lints: new_flags += '\x1f-Aunknown-lints'
    environ[var] = pfx + new_flags
def set_warn_vars(**kw):
    global SAVED_ENC_RUSTFLAGS, SAVED_ENC_RUSTDOCFLAGS
    _set_warn_var('CARGO_ENCODED_RUSTFLAGS', SAVED_ENC_RUSTFLAGS, **kw)
    _set_warn_var('CARGO_ENCODED_RUSTDOCFLAGS', SAVED_ENC_RUSTDOCFLAGS, **kw)

def suite(vers, *, target=None, test=True):
    if not HAS_RUSTUP: vers = None
    target = [] if target is None else ['--target', target]
    check = 'clippy' if HAS_CLIPPY else 'check'
    checkargs = target + ['--all-targets']
    testargs = target
    docargs = target + ['--no-deps']
    ftokio = ['--features', 'tokio']

    set_warn_vars(allow_unknown_lints = vers != 'nightly')

    with ErasingLine():
        cargo(vers, 'Check for default config', check, checkargs)
        if test: cargo(vers, "Tests for default config", 'test', testargs, capture=True)
        cargo(vers, 'Rustdoc for default config', 'doc', docargs)
        cargo(vers, 'Check for Tokio config', check, ftokio + checkargs)
        if test: cargo(vers, "Tests for Tokio config", 'test', ftokio + testargs, capture=True)
        cargo(vers, 'Rustdoc for Tokio config', 'doc', ftokio + docargs)
    print(f'Suite {green("succeeded")}', f' for {blue(vers)}' if vers is not None else '', sep='')

def suites(*, target=None, test=True):
    print(f'{yellow(HOST_TARGET_STRING)} (host target)' if target is None else yellow(target))
    with Indent():
        suite(MSRV)
        if HAS_RUSTUP: suite('nightly')

def main():
    global MANIFEST_DIR, MSRV, HAS_RUSTUP, HAS_CLIPPY
    global HOST_TARGET, HOST_TARGET_STRING, HOST_TARGET_0, HOST_TARGET_1, HOST_TARGET_2, HOST_TARGET_3
    if len(sys.argv) > 1:
        print(ERROR, 'too many arguments')
        sys.exit(1)

    MANIFEST_DIR = Path(__file__).parent
    MSRV = read_msrv()

    with ErasingLine():
        print(italic('Preparing toolchain…'))
        HAS_RUSTUP = check_rustup()
        HAS_CLIPPY = check_clippy()
        print('Clippy is', green('available') if HAS_CLIPPY else red('not available'), erase=False)

        print(italic('Determining host target…'))
        HOST_TARGET_STRING = get_host_target()
        HOST_TARGET = HOST_TARGET_STRING.split('-')
        HOST_TARGET_0, HOST_TARGET_1, HOST_TARGET_2, HOST_TARGET_3 = \
            HOST_TARGET + [''] * (4 - len(HOST_TARGET) if len(HOST_TARGET) < 4 else 0)

        cargo(None, 'Fetching dependencies', 'fetch', frozen=False)

    suites()
    if HOST_TARGET_0 == 'x86_64' and HOST_TARGET_2 in ['windows', 'linux']:
        suites(target='-'.join(['i686'] + HOST_TARGET[1:]), test=True)

    print(f'Discontinuous integration {green("succeeded")}')

if __name__ == '__main__':
    try: main()
    except RuntimeError as e: print(ERROR, str(e))
    except KeyboardInterrupt: sys.exit(2)
