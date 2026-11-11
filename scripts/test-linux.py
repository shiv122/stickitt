#!/usr/bin/env python3
"""Linux desktop regression checks in a private D-Bus session and disposable nested KWin."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
PORTAL_PATH = '/org/freedesktop/portal/desktop'
SHORTCUTS = 'org.freedesktop.portal.GlobalShortcuts'


def wait_for(check, description, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f'Timed out: {description}')


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, timeout=10, **kwargs).stdout


def mock_portal(directory):
    # The mock implements the real portal protocol, including directed signals and requests.
    from gi.repository import Gio, GLib
    connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    state = {'bindings': [], 'closed': 0, 'items': [], 'app_id': None}
    sessions = {}
    registrations = []
    scheme = 1

    def save():
        path = directory / 'portal.json'
        temporary = path.with_suffix('.tmp')
        temporary.write_text(json.dumps(state))
        temporary.replace(path)

    def register(path, xml, callback, properties=None):
        info = Gio.DBusNodeInfo.new_for_xml('<node>' + xml + '</node>')
        for interface in info.interfaces:
            registrations.append(connection.register_object(path, interface, callback, properties, None))

    def reply(sender, token, values):
        path = f'{PORTAL_PATH}/request/{sender[1:].replace(".", "_")}/{token}'

        def response():
            connection.emit_signal(sender, path, 'org.freedesktop.portal.Request', 'Response', GLib.Variant('(ua{sv})', (0, values)))
            return False

        GLib.timeout_add(20, response)
        return GLib.Variant('(o)', (path,))

    def session_call(conn, sender, path, interface, method, parameters, invocation):
        if method == 'Close':
            sessions.pop(path, None)
            state['closed'] += 1
            save()
            invocation.return_value(None)
            conn.emit_signal(sender, path, interface, 'Closed', GLib.Variant('(a{sv})', ({},)))

    def call(conn, sender, path, interface, method, parameters, invocation):
        nonlocal scheme
        args = parameters.unpack()
        if interface == SHORTCUTS:
            if method == 'CreateSession':
                options = args[0]
                session = f'{PORTAL_PATH}/session/{sender[1:].replace(".", "_")}/{options["session_handle_token"]}'
                sessions[session] = sender
                register(session, '<interface name="org.freedesktop.portal.Session"><method name="Close"/><signal name="Closed"><arg type="a{sv}"/></signal></interface>', session_call)
                invocation.return_value(reply(sender, options['handle_token'], {'session_handle': GLib.Variant('s', session)}))
            elif method == 'BindShortcuts':
                session, shortcuts, parent, options = args
                state['bindings'] = [item[0] for item in shortcuts]
                state['triggers'] = {key: values.get('preferred_trigger') for key, values in shortcuts}
                save()
                result = [(key, {'description': GLib.Variant('s', values['description']), 'trigger_description': GLib.Variant('s', values.get('preferred_trigger', ''))}) for key, values in shortcuts]
                invocation.return_value(reply(sender, options['handle_token'], {'shortcuts': GLib.Variant('a(sa{sv})', result)}))
        elif interface == 'org.freedesktop.host.portal.Registry':
            state['app_id'] = args[0]
            save()
            invocation.return_value(None)
        elif interface == 'org.freedesktop.portal.Settings':
            invocation.return_value(GLib.Variant('(v)', (GLib.Variant('u', scheme),)))
        elif interface == 'dev.stickitt.TestPortal':
            if method == 'Activate':
                assert args[0] in state['bindings'], args[0]
                for session, owner in sessions.items():
                    conn.emit_signal(owner, PORTAL_PATH, SHORTCUTS, 'Activated', GLib.Variant('(osta{sv})', (session, args[0], 0, {})))
            elif method == 'Theme':
                scheme = args[0]
                conn.emit_signal(None, PORTAL_PATH, 'org.freedesktop.portal.Settings', 'SettingChanged', GLib.Variant('(ssv)', ('org.freedesktop.appearance', 'color-scheme', GLib.Variant('u', scheme))))
            invocation.return_value(None)

    register(PORTAL_PATH, '''
      <interface name="org.freedesktop.portal.GlobalShortcuts">
        <property name="version" type="u" access="read"/>
        <method name="CreateSession"><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
        <method name="BindShortcuts"><arg type="o" direction="in"/><arg type="a(sa{sv})" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
        <signal name="Activated"><arg type="o"/><arg type="s"/><arg type="t"/><arg type="a{sv}"/></signal>
      </interface>
      <interface name="org.freedesktop.host.portal.Registry">
        <property name="version" type="u" access="read"/>
        <method name="Register"><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
      </interface>
      <interface name="org.freedesktop.portal.Settings">
        <property name="version" type="u" access="read"/>
        <method name="Read"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
        <method name="ReadOne"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
        <signal name="SettingChanged"><arg type="s"/><arg type="s"/><arg type="v"/></signal>
      </interface>
      <interface name="dev.stickitt.TestPortal">
        <method name="Activate"><arg type="s" direction="in"/></method>
        <method name="Theme"><arg type="u" direction="in"/></method>
      </interface>''', call, lambda *args: GLib.Variant('u', 2))

    def watcher_call(conn, sender, path, interface, method, parameters, invocation):
        service = parameters.unpack()[0]
        state['items'].append({'owner': sender if service.startswith('/') else service, 'path': service if service.startswith('/') else '/StatusNotifierItem'})
        save()
        invocation.return_value(None)

    def watcher_property(conn, sender, path, interface, name):
        if name == 'IsStatusNotifierHostRegistered':
            return GLib.Variant('b', True)
        if name == 'RegisteredStatusNotifierItems':
            return GLib.Variant('as', [item['owner'] + item['path'] for item in state['items']])
        return GLib.Variant('i', 0)

    register('/StatusNotifierWatcher', '''<interface name="org.kde.StatusNotifierWatcher">
      <method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
      <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
      <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
      <property name="ProtocolVersion" type="i" access="read"/>
      </interface>''', watcher_call, watcher_property)
    save()
    Gio.bus_own_name_on_connection(connection, 'org.kde.StatusNotifierWatcher', Gio.BusNameOwnerFlags.NONE, None, None)
    Gio.bus_own_name_on_connection(connection, 'org.freedesktop.portal.Desktop', Gio.BusNameOwnerFlags.NONE, lambda *args: (directory / 'portal.ready').touch(), None)
    GLib.MainLoop().run()


def inside(binary, directory):
    from gi.repository import Gio, GLib
    from Xlib import X, XK, display, protocol
    from Xlib.ext import shape
    connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    d = display.Display()
    root = d.screen().root
    checks = []
    children = []

    def dbus(service, path, interface, method, parameters=None):
        return connection.call_sync(service, path, interface, method, parameters, None, Gio.DBusCallFlags.NONE, 5000, None)

    def portal(action):
        dbus('org.freedesktop.portal.Desktop', PORTAL_PATH, 'dev.stickitt.TestPortal', 'Activate', GLib.Variant('(s)', (action,)))

    def workspaces(pid):
        return json.loads(dbus(f'dev.stickitt.Windows.p{pid}', '/dev/stickitt/Windows', 'dev.stickitt.Windows', 'Snapshot').unpack()[0])

    def prop(window, name):
        result = window.get_full_property(d.intern_atom(name), X.AnyPropertyType)
        return result.value if result is not None else []

    def windows(pid):
        return [d.create_resource_object('window', int(w)) for w in prop(root, '_NET_CLIENT_LIST') if list(prop(d.create_resource_object('window', int(w)), '_NET_WM_PID')) == [pid]]

    def notes(pid):
        skip = d.intern_atom('_NET_WM_STATE_SKIP_TASKBAR')
        return [w for w in windows(pid) if skip not in prop(w, '_NET_WM_STATE')]

    def records():
        with sqlite3.connect(directory / 'notes.db') as db:
            return db.execute('SELECT body,mode,desktop,hidden,view FROM notes WHERE deleted_at IS NULL ORDER BY id').fetchall()

    def focus(window):
        run('xdotool', 'windowactivate', '--sync', str(window.id))

    def key(window, name, modifiers=0):
        # XTEST is routed through libei on modern KWin. Send core key events directly to our
        # own window instead; this also avoids changing another application's input state.
        code = d.keysym_to_keycode(XK.string_to_keysym(name))
        assert code, name
        values = dict(time=X.CurrentTime, root=root, window=window, child=X.NONE,
                      root_x=0, root_y=0, event_x=90, event_y=110, state=modifiers,
                      same_screen=1, detail=code)
        window.send_event(protocol.event.KeyPress(**values), event_mask=X.KeyPressMask)
        window.send_event(protocol.event.KeyRelease(**values), event_mask=X.KeyReleaseMask)
        d.sync()

    def type_text(window, text):
        for char in text:
            key(window, 'space' if char == ' ' else char, X.ShiftMask if char.isupper() else 0)

    def stop(process):
        if process.poll() is None:
            visible = [w for w in notes(process.pid) if w.get_attributes().map_state == X.IsViewable]
            if visible:
                focus(visible[-1])
                key(visible[-1], 'q', X.ControlMask)
            process.wait(timeout=5)
        assert process.returncode == 0, f'application exited {process.returncode}'
        assert not dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', GLib.Variant('(s)', (f'stickitt-{process.pid}-watch',))).unpack()[0]

    log = (directory / 'app.log').open('w')
    mock_log = (directory / 'mock.log').open('w')
    mock = subprocess.Popen([sys.executable, __file__, '--portal', '--artifacts', str(directory)], stdout=mock_log, stderr=subprocess.STDOUT)
    children.append(mock)
    try:
        wait_for(lambda: (directory / 'portal.ready').exists(), 'private mock portal')
        env = os.environ.copy()
        env['XDG_SESSION_TYPE'] = 'wayland'
        env['STICKIT_DEBUG'] = '1'
        args = [str(binary), '--db', str(directory / 'notes.db'), '--config', str(directory / 'stickit.ini')]
        app = subprocess.Popen(args, env=env, stdout=log, stderr=subprocess.STDOUT)
        children.append(app)
        first = wait_for(lambda: notes(app.pid), 'welcome note')[0]
        wait_for(lambda: len(json.loads((directory / 'portal.json').read_text())['bindings']) == 6, 'portal shortcut binding')
        checks.append('Wayland-session startup, GTK tray and six portal bindings')
        focus(first)
        key(first, 'a', X.ControlMask)
        type_text(first, 'Linux integration note')
        wait_for(lambda: records()[0][0] == 'Linux integration note', 'edited note persisted')
        assert subprocess.run(args, env=env, capture_output=True, timeout=5).returncode == 0
        assert len(notes(app.pid)) == 1
        checks.append('editing, SQLite persistence and single-instance relaunch')

        portal('new_note')
        wait_for(lambda: len(notes(app.pid)) == 2, 'portal activation forwarded into winit')
        item = wait_for(lambda: json.loads((directory / 'portal.json').read_text())['items'], 'StatusNotifier registration')[0]
        menu = dbus(item['owner'], item['path'], 'org.freedesktop.DBus.Properties', 'Get', GLib.Variant('(ss)', ('org.kde.StatusNotifierItem', 'Menu'))).unpack()[0]
        layout = dbus(item['owner'], menu, 'com.canonical.dbusmenu', 'GetLayout', GLib.Variant('(iias)', (0, -1, []))).unpack()[1]

        def find_label(node, label):
            identity, properties, descendants = node
            if properties.get('label', '').replace('_', '') == label:
                return identity
            for child in descendants:
                result = find_label(child, label)
                if result is not None:
                    return result

        new_id = find_label(layout, 'New note')
        assert new_id is not None, layout
        dbus(item['owner'], menu, 'com.canonical.dbusmenu', 'Event', GLib.Variant('(isvu)', (new_id, 'clicked', GLib.Variant('i', 0), 0)))
        wait_for(lambda: len(notes(app.pid)) == 3, 'GTK menu event forwarded into winit')
        checks.append('real exported GTK tray menu creates a note')

        clipboard = subprocess.Popen(['wl-copy', '--foreground', '--type', 'text/plain'], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=log)
        children.append(clipboard)
        clipboard.stdin.write(b'Linux clipboard fixture')
        clipboard.stdin.close()
        time.sleep(0.15)
        portal('paste_as_note')
        wait_for(lambda: any(row[0] == 'Linux clipboard fixture' for row in records()), 'native Wayland clipboard to note')
        portal('toggle_notes')
        wait_for(lambda: all(w.get_attributes().map_state != X.IsViewable for w in notes(app.pid)), 'hide all notes')
        portal('toggle_notes')
        wait_for(lambda: all(w.get_attributes().map_state == X.IsViewable for w in notes(app.pid)), 'restore all notes')
        checks.append('native Wayland clipboard, hide and restore')

        focus(first)
        key(first, 't', X.ControlMask)
        wait_for(lambda: d.intern_atom('_NET_WM_STATE_ABOVE') in prop(first, '_NET_WM_STATE'), 'always on top')
        key(first, 't', X.ControlMask)
        wait_for(lambda: d.intern_atom('_NET_WM_STATE_BELOW') in prop(first, '_NET_WM_STATE'), 'desktop pinning')
        key(first, 't', X.ControlMask)
        wait_for(lambda: d.intern_atom('_NET_WM_STATE_BELOW') not in prop(first, '_NET_WM_STATE'), 'normal stacking restored')
        checks.append('normal, always-on-top and desktop window modes')

        geometry = first.get_geometry()
        assert geometry.depth == 32, 'ARGB32 visual is required for transparency'
        rectangles = first.shape_get_rectangles(shape.SK.Input).rectangles
        assert rectangles and not any(r.x == 0 and r.y == 0 and r.width == geometry.width and r.height == geometry.height for r in rectangles)
        def opaque_frame():
            image = first.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xFFFFFFFF)
            return image if max(image.data[3::4]) >= 250 else None
        image = wait_for(opaque_frame, 'note fade-in finishes')
        assert image.data[3] == 0, 'transparent corner'
        if importlib.util.find_spec('PIL'):
            from PIL import Image
            Image.frombytes('RGBA', (geometry.width, geometry.height), image.data, 'raw', 'BGRA').save(directory / 'note.png')
        checks.append('ARGB transparency and click-through window shape')
        center = ((geometry.height - 65) * geometry.width + geometry.width // 2) * 4
        before = sum(image.data[center:center+3])
        dbus('org.freedesktop.portal.Desktop', PORTAL_PATH, 'dev.stickitt.TestPortal', 'Theme', GLib.Variant('(u)', (2,)))
        wait_for(lambda: sum(first.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xFFFFFFFF).data[center:center+3]) > before + 100, 'system theme updates rendered notes')
        checks.append('desktop portal theme changes repaint cached note styles')

        dbus('org.kde.KWin', '/VirtualDesktopManager', 'org.kde.KWin.VirtualDesktopManager', 'createDesktop', GLib.Variant('(us)', (1, 'Integration two')))
        desktops = wait_for(lambda: workspaces(app.pid)['desktops'] if len(workspaces(app.pid)['desktops']) == 2 else None, 'second disposable desktop')
        first_desktop, second_desktop = [item[0] for item in desktops]
        dbus('org.kde.KWin', '/VirtualDesktopManager', 'org.freedesktop.DBus.Properties', 'Set', GLib.Variant('(ssv)', ('org.kde.KWin.VirtualDesktopManager', 'current', GLib.Variant('s', second_desktop))))
        wait_for(lambda: workspaces(app.pid)['current'] == second_desktop, 'switch disposable desktop')
        portal('new_note')
        wait_for(lambda: len(notes(app.pid)) == 5, 'new note on second desktop')
        wait_for(lambda: any(w['name'].endswith('-dock') and not w['desktops'] for w in workspaces(app.pid)['windows']), 'dock on every KDE workspace')
        stop(app)
        assert {row[2] for row in records()} == {first_desktop, second_desktop}, records()

        # Exercise actual due-reminder handling, but replace sound players in this private
        # process so tests never play audio on the user's speakers.
        fakebin = directory / 'fakebin'
        fakebin.mkdir()
        themed = fakebin / 'canberra-gtk-play'
        themed.write_text(f'#!{sys.executable}\nimport sys\nfrom pathlib import Path\nPath({str(directory / "theme-sound.json")!r}).write_text(__import__("json").dumps(sys.argv[1:]))\nsys.exit(1)\n')
        fallback = fakebin / 'pw-play'
        fallback.write_text(f'#!{sys.executable}\nimport sys\nfrom pathlib import Path\nPath({str(directory / "chime.wav")!r}).write_bytes(sys.stdin.buffer.read())\n')
        for helper in (themed, fallback):
            helper.chmod(0o755)
        env['PATH'] = str(fakebin) + os.pathsep + env['PATH']
        with sqlite3.connect(directory / 'notes.db') as database:
            note_id = database.execute('SELECT MAX(id) FROM notes').fetchone()[0]
            database.execute('UPDATE notes SET reminder_due=?, reminder_anchor=? WHERE id=?', (int(time.time()) - 1, int(time.time()) - 1, note_id))
        app = subprocess.Popen(args, env=env, stdout=log, stderr=subprocess.STDOUT)
        children.append(app)
        restored = wait_for(lambda: notes(app.pid) if len(notes(app.pid)) == 5 else None, 'restart restores five notes')
        wait_for(lambda: {desk for w in workspaces(app.pid)['windows'] if '-note-' in w['name'] for desk in w['desktops']} == {first_desktop, second_desktop}, 'KDE workspace positions restored')
        wait_for(lambda: (directory / 'chime.wav').exists(), 'reminder sound fallback')
        assert (directory / 'chime.wav').read_bytes() == (ROOT / 'assets/reminder.wav').read_bytes()
        assert json.loads((directory / 'theme-sound.json').read_text())[:2] == ['--id', 'message']
        compact = next(w for w in restored if f'-note-{note_id}'.encode() in prop(w, 'WM_CLASS'))
        focus(compact)
        key(compact, 'm', X.ControlMask | X.ShiftMask)
        wait_for(lambda: records()[-1][4] == 1, 'compact view persisted')
        stop(app)
        checks.append('per-desktop persistence, restart and dock on every desktop')
        checks.append('due reminder uses themed sound and bundled WAV fallback')
        checks.append('compact view works and persists')
        wait_for(lambda: json.loads((directory / 'portal.json').read_text())['closed'] >= 1, 'portal session closed')
        errors = (directory / 'app.log').read_text()
        assert not any(marker in errors for marker in ('panicked', 'Gtk-CRITICAL', 'Protocol error', 'unavailable', 'not granted')), errors
        result = {'checks': checks, 'display': os.environ['DISPLAY'], 'database': str(directory / 'notes.db')}
        (directory / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2), flush=True)
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        log.close()
        mock_log.close()


def nested(binary, directory):
    for program in ('dbus-run-session', 'kwin_wayland', 'xdotool', 'wl-copy'):
        assert shutil.which(program), f'Install test dependency: {program}'
    for module in ('gi', 'Xlib'):
        assert importlib.util.find_spec(module), f'Install Python test dependency: {module}'
    runtime = directory / 'runtime'
    runtime.mkdir(mode=0o700)
    script = directory / 'session.sh'
    command = [sys.executable, str(Path(__file__).resolve()), '--inside', '--binary', str(binary), '--artifacts', str(directory)]
    script.write_text('#!/bin/sh\nexec ' + shlex.join(command) + '\n')
    script.chmod(0o755)
    env = os.environ.copy()
    for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'SESSION_MANAGER'):
        env.pop(name, None)
    env.update(XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(directory / 'config'), XDG_DATA_HOME=str(directory / 'data'), XDG_CACHE_HOME=str(directory / 'cache'), XDG_CURRENT_DESKTOP='KDE', XDG_SESSION_TYPE='wayland', QT_QPA_PLATFORM='offscreen', KWIN_COMPOSE='Q', LIBGL_ALWAYS_SOFTWARE='1', XWAYLAND_NO_GLAMOR='1')
    with (directory / 'compositor.log').open('w') as log:
        process = subprocess.Popen(['dbus-run-session', '--', 'kwin_wayland', '--virtual', '--xwayland', '--no-lockscreen', '--no-kactivities', '--width', '1280', '--height', '900', '--exit-with-session', str(script)], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            process.wait(timeout=90)
        finally:
            # Also reap helpers activated by the private bus, never touch the user's desktop.
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        assert process.returncode == 0, (directory / 'compositor.log').read_text()[-5000:]
    assert (directory / 'result.json').exists(), (directory / 'compositor.log').read_text()[-5000:]
    print((directory / 'result.json').read_text())
    print(f'Artifacts: {directory}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/stickit')
    parser.add_argument('--artifacts', type=Path)
    parser.add_argument('--inside', action='store_true')
    parser.add_argument('--portal', action='store_true')
    args = parser.parse_args()
    directory = args.artifacts or Path(tempfile.mkdtemp(prefix='stickitt-linux-test-'))
    directory.mkdir(parents=True, exist_ok=True)
    if args.portal:
        mock_portal(directory)
    elif args.inside:
        inside(args.binary.resolve(), directory.resolve())
    else:
        nested(args.binary.resolve(), directory.resolve())
