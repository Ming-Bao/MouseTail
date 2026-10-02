// MouseTail in GNOME's top bar: the MouseTail icon, and a menu with what the Omarchy bar
// plugin's panel has: who's connected (pair, pause, forget), Arrange Displays (arrange.js), any
// pairing code, the settings, and updates. Status streams from `mousetail watch`, one JSON line
// per change; everything else is the `mousetail` command.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import * as Status from './status.js';

const BINARY = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'mousetail']);
const HELPERS = GLib.build_filenamev([GLib.get_user_data_dir(), 'mousetail']);

/** Run `argv`, then `done(ok, stdout, stderr)`, unless `cancellable` was cancelled first. */
function run(argv, cancellable, done) {
    let proc;
    try {
        proc = Gio.Subprocess.new(argv,
            Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE);
    } catch (e) {
        done(false, '', e.message);
        return;
    }
    proc.communicate_utf8_async(null, cancellable, (p, result) => {
        if (cancellable.is_cancelled())
            return;
        let out = '', err = '';
        try {
            [, out, err] = p.communicate_utf8_finish(result);
        } catch (e) {
            err = e.message;
        }
        done(p.get_successful(), out ?? '', err ?? '');
    });
}

/** Call `onLine` with each line `stream` gives, then `onEnd`, unless cancelled. */
function readLines(stream, cancellable, onLine, onEnd = () => {}) {
    const lines = new Gio.DataInputStream({base_stream: stream, close_base_stream: true});
    const next = () => lines.read_line_async(GLib.PRIORITY_DEFAULT, cancellable, (s, result) => {
        if (cancellable.is_cancelled())
            return;
        let line = null;
        try {
            [line] = s.read_line_finish_utf8(result);
        } catch {
            // The process has gone.
        }
        if (line === null) {
            onEnd();
            return;
        }
        onLine(line);
        next();
    });
    next();
}

/** What a command said on stderr, as the panel shows it. */
function said(text) {
    return text.trim().replace(/^mousetail: /, '');
}

/** Wrapped text, as wide as the menu. Dimmed through opacity, so it suits light and dark. */
function note(text, styleClass = 'mousetail-note') {
    const label = new St.Label({text, style_class: styleClass, x_expand: true});
    label.clutter_text.line_wrap = true;
    label.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
    label.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
    if (styleClass === 'mousetail-note')
        label.opacity = 165;
    return label;
}

/** A menu row holding other things (buttons, an entry), which isn't clickable itself: clicking
 * it, or a button in it, leaves the menu open. Reactive, though, as GNOME greys out rows that
 * aren't; and not highlighted on hover. */
function row(...children) {
    const item = new PopupMenu.PopupBaseMenuItem({activate: false, hover: false, can_focus: false});
    item.track_hover = false;
    children.forEach(child => item.add_child(child));
    return item;
}

function button(label, onClick) {
    const b = new St.Button({
        label,
        style_class: 'button mousetail-button',
        can_focus: true,
        y_align: Clutter.ActorAlign.CENTER,
    });
    b.connect('clicked', onClick);
    return b;
}

function iconButton(iconName, accessibleName, onClick, styleClass = '') {
    const b = new St.Button({
        style_class: `button mousetail-icon-button ${styleClass}`,
        can_focus: true,
        accessible_name: accessibleName,
        y_align: Clutter.ActorAlign.CENTER,
        child: new St.Icon({icon_name: iconName, icon_size: 16}),
    });
    b.connect('clicked', onClick);
    return b;
}

// A switch that leaves the menu open, like the Omarchy panel's.
const SettingItem = GObject.registerClass({GTypeName: 'MouseTailSettingItem'},
class SettingItem extends PopupMenu.PopupSwitchMenuItem {
    activate() {
        this.toggle();
    }
});

const Indicator = GObject.registerClass({GTypeName: 'MouseTailIndicator'},
class Indicator extends PanelMenu.Button {
    _init(path) {
        super._init(0.5, 'MouseTail');
        this._logo = Gio.icon_new_for_string(`${path}/icons/mousetail-symbolic.svg`);
        this.add_child(new St.Icon({gicon: this._logo, style_class: 'system-status-icon'}));
        this.menu.box.add_style_class_name('mousetail-menu');

        this._status = {running: false};
        this._message = '';
        this._atLogin = true;
        this._queue = [];
        this._busy = false;
        this._shown = new Map();
        this._cancellable = new Gio.Cancellable();

        this._build();
        this._update();
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._opened();
        });
        this.connect('destroy', () => this._stop());
        this._watch();
    }

    _build() {
        const menu = this.menu;

        const title = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        title.add_child(new St.Label({text: 'MouseTail', style_class: 'mousetail-title'}));
        this._summary = new St.Label({style_class: 'mousetail-summary'});
        this._summary.opacity = 165;
        title.add_child(this._summary);
        menu.addMenuItem(row(
            new St.Icon({gicon: this._logo, icon_size: 32, style_class: 'mousetail-logo'}),
            title));

        // Anything stopping MouseTail doing its job here; the pairing code (Task 3).
        this._problems = new PopupMenu.PopupMenuSection();
        menu.addMenuItem(this._problems);
        this._code = new PopupMenu.PopupMenuSection();
        menu.addMenuItem(this._code);

        this._computersHeading = new PopupMenu.PopupSeparatorMenuItem('Computers');
        menu.addMenuItem(this._computersHeading);
        this._computers = new PopupMenu.PopupMenuSection();
        menu.addMenuItem(this._computers);
        this._arrange = new PopupMenu.PopupMenuItem('Arrange Displays…');
        this._arrange.connect('activate', () => this._openArrange());
        menu.addMenuItem(this._arrange);

        this._settingsHeading = new PopupMenu.PopupSeparatorMenuItem('Settings');
        menu.addMenuItem(this._settingsHeading);
        this._switches = [
            ['Sound follows you', 'audio'],
            ['Share clipboard', 'clipboard'],
            ['Ripple when crossing', 'ripple'],
            ['Start at login', null],
            ['Update automatically', 'updates'],
        ].map(([label, key]) => {
            const item = new SettingItem(label, true);
            item.connect('toggled', (_item, on) => {
                if (this._syncing)
                    return;
                if (key)
                    this._command([BINARY, 'set', key, on ? 'on' : 'off']);
                else
                    this._setAtLogin(on);
            });
            menu.addMenuItem(item);
            return {item, key};
        });

        this._notRunning = row(note("MouseTail isn't running."));
        menu.addMenuItem(this._notRunning);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._messageLabel = note('');
        this._messageRow = row(this._messageLabel);
        menu.addMenuItem(this._messageRow);
        this._updatesButton = button('Check for Updates', () => this._checkForUpdates());
        this._startStop = button('Stop MouseTail', () => this._command(
            ['systemctl', '--user', this._status.running ? 'stop' : 'start', 'mousetail']));
        const actions = new St.BoxLayout({style_class: 'mousetail-actions', x_expand: true});
        actions.add_child(this._updatesButton);
        actions.add_child(this._startStop);
        menu.addMenuItem(row(actions));
        this._versionLabel = note('');
        this._versionRow = row(this._versionLabel);
        menu.addMenuItem(this._versionRow);
    }

    /** Make the menu say what the status says. */
    _update() {
        const st = this._status;
        const running = st.running === true;
        this._summary.text = Status.summary(st);

        this._computersHeading.visible = running;
        this._arrange.visible = running && Status.shownPeers(st).some(p => p.paired);
        this._settingsHeading.visible = running;
        this._syncing = true;
        for (const {item, key} of this._switches) {
            item.visible = running;
            item.setToggleState(key ? Status.setting(st, key) : this._atLogin);
        }
        this._syncing = false;

        this._notRunning.visible = !running;
        this._messageLabel.text = this._message;
        this._messageRow.visible = this._message !== '';
        this._updatesButton.visible = running;
        this._startStop.label = running ? 'Stop MouseTail' : 'Start MouseTail';
        this._versionLabel.text = st.version ? `MouseTail ${st.version}` : '';
        this._versionRow.visible = !!st.version;
    }

    /** Fill `section` with `items()` when what it shows (`key`) has changed, so an unrelated
     * update doesn't sweep away a field being typed in. A falsy key empties it. */
    _refill(section, key, items) {
        const shows = JSON.stringify(key || null);
        if (this._shown.get(section) === shows)
            return;
        this._shown.set(section, shows);
        section.removeAll();
        if (key)
            items().forEach(item => section.addMenuItem(item));
    }

    _opened() {
        this._message = '';
        run(['systemctl', '--user', 'is-enabled', 'mousetail'], this._cancellable, (_ok, out) => {
            this._atLogin = out.trim() === 'enabled';
            this._update();
        });
        this._update();
    }

    _openArrange() {
        // Task 4.
    }

    // -------------------------------------------------------------- commands

    /** Run a command once those before it have finished; what it says goes in the menu. */
    _command(argv) {
        this._queue.push(argv);
        if (!this._busy)
            this._next();
    }

    _next() {
        const argv = this._queue.shift();
        this._busy = argv !== undefined;
        if (!this._busy)
            return;
        run(argv, this._cancellable, (_ok, _out, err) => {
            if (said(err) !== '') {
                this._message = said(err);
                this._update();
            }
            this._next();
        });
    }

    _setAtLogin(on) {
        this._atLogin = on;
        this._command(['systemctl', '--user', on ? 'enable' : 'disable', 'mousetail']);
    }

    _checkForUpdates() {
        this._message = 'Checking for updates…';
        this._updatesButton.reactive = false;
        this._update();
        run([BINARY, 'update'], this._cancellable, (_ok, out, err) => {
            this._updatesButton.reactive = true;
            this._message = said(err) || out.trim();
            this._update();
        });
    }

    // -------------------------------------------------------------- status

    _watch() {
        let proc;
        try {
            proc = Gio.Subprocess.new([BINARY, 'watch'],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_SILENCE);
        } catch {
            // Not installed (yet).
            this._watchAgain();
            return;
        }
        this._watcher = proc;
        readLines(proc.get_stdout_pipe(), this._cancellable, line => {
            const status = Status.parse(line);
            if (status)
                this._setStatus(status);
        });
        proc.wait_async(this._cancellable, (p, result) => {
            try {
                p.wait_finish(result);
            } catch {
                return; // Cancelled: the extension is going away.
            }
            this._watcher = null;
            this._setStatus({running: false});
            this._watchAgain();
        });
    }

    _watchAgain() {
        this._watchTimer = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 3, () => {
            this._watchTimer = 0;
            this._watch();
            return GLib.SOURCE_REMOVE;
        });
    }

    _setStatus(status) {
        this._status = status;
        this._update();
    }

    _stop() {
        this._cancellable.cancel();
        this._watcher?.force_exit();
        if (this._watchTimer)
            GLib.source_remove(this._watchTimer);
    }
});

export default class MouseTailExtension extends Extension {
    enable() {
        this._indicator = new Indicator(this.path);
        Main.panel.addToStatusArea(this.uuid, this._indicator);
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
