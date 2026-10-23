// Atoll Bridge: the one thing only GNOME Shell can do for Atoll, opening a
// notification as if its banner was clicked (Discord then opens the exact
// chat). Atoll calls one D-Bus method; this file adds no UI and no timers.
//
// It runs inside GNOME Shell, so every path is guarded: a failure answers
// "false" (Atoll then just brings the app forward) and never throws into
// the Shell.

import Gio from 'gi://Gio';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const PATH = '/dev/atoll/Bridge';
const IFACE = `
<node>
  <interface name="dev.atoll.Bridge">
    <method name="ActivateNotification">
      <arg type="s" direction="in" name="app"/>
      <arg type="s" direction="in" name="title"/>
      <arg type="s" direction="in" name="body"/>
      <arg type="b" direction="out" name="found"/>
    </method>
  </interface>
</node>`;

// Atoll sees bodies without markup; compare text only.
function plain(s) {
    return (s ?? '').replace(/<[^>]*>/g, '').replace(/\s+/g, ' ').trim();
}

export default class AtollBridge {
    enable() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        // GNOME Shell's own connection: Atoll reaches it as org.gnome.Shell.
        this._dbus.export(Gio.DBus.session, PATH);
    }

    disable() {
        this._dbus?.unexport();
        this._dbus = null;
    }

    ActivateNotification(app, title, body) {
        try {
            const wanted = {app: plain(app), title: plain(title), body: plain(body)};
            let best = null;
            let bestScore = 0;
            for (const source of Main.messageTray.getSources()) {
                for (const n of source.notifications ?? []) {
                    if (plain(n.title) !== wanted.title)
                        continue;
                    let score = 1;
                    if (wanted.body && plain(n.body) === wanted.body)
                        score += 2;
                    if (wanted.app && plain(source.title) === wanted.app)
                        score += 1;
                    // Newest wins a tie: the one Atoll shows at the top.
                    if (score > bestScore || (score === bestScore && best &&
                        n.datetime?.compare(best.datetime) > 0)) {
                        best = n;
                        bestScore = score;
                    }
                }
            }
            if (!best)
                return false;
            best.activate();
            return true;
        } catch (e) {
            console.warn(`Atoll Bridge: ${e}`);
            return false;
        }
    }
}
