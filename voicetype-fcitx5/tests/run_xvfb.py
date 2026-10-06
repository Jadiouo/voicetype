#!/usr/bin/env python3
"""Launch tests only in our own X server; never inject into inherited DISPLAY."""
import os, select, signal, subprocess, sys, tempfile
xvfb, executable, *args = sys.argv[1:]
with tempfile.TemporaryDirectory(prefix='voicetype-caps-test-') as temp:
    rd, wr = os.pipe()
    server = subprocess.Popen([xvfb, '-displayfd', str(wr), '-screen', '0', '800x600x24', '-nolisten', 'tcp'], pass_fds=[wr], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    os.close(wr)
    try:
        if not select.select([rd], [], [], 5)[0]: raise RuntimeError('Xvfb startup timeout')
        number = os.read(rd, 64).decode().strip()
        if not number.isdecimal(): raise RuntimeError('invalid isolated display')
        display = ':' + number
        if display == os.environ.get('DISPLAY'): raise RuntimeError('refusing user display')
        env = os.environ.copy()
        env.update(DISPLAY=display, XDG_RUNTIME_DIR=temp, FCITX_CONFIG_HOME=temp, FCITX_DATA_HOME=temp, VOICETYPE_ISOLATED_DISPLAY='1', VOICETYPE_SOCKET=temp+'/absent.sock')
        subprocess.run(['setxkbmap', '-display', display, '-layout', 'us', '-option', ''], env=env, check=True)
        result = subprocess.run([executable, *args], env=env, timeout=15)
    finally:
        os.close(rd)
        os.killpg(server.pid, signal.SIGTERM)
        try: server.wait(timeout=3)
        except subprocess.TimeoutExpired: os.killpg(server.pid, signal.SIGKILL); server.wait()
raise SystemExit(result.returncode)
