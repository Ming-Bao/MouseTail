import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// MouseTail in the Omarchy bar: the MouseTail icon, and a panel with what the Mac's menu has:
// who's connected (pair, pause, forget), Arrange Displays (ArrangeWindow.qml), any pairing
// code, the settings, and updates. Status streams from `mousetail watch`, one JSON line per change;
// everything else is the `mousetail` command.
Panel {
  id: root
  moduleName: "nz.galengreen.mousetail"
  ipcTarget: "nz.galengreen.mousetail"

  readonly property string binary: Quickshell.env("HOME") + "/.local/bin/mousetail"

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  property bool running: false
  property var status: ({})

  readonly property var peers: (status.peers || []).filter(function(p) { return p.paired || p.connected })
  readonly property var connectedPeers: peers.filter(function(p) { return p.paired && p.connected && !p.paused })
  readonly property string controlledBy: nameOf(status.controlled_by)
  readonly property var pairingCode: status.pairing_code || null
  readonly property bool clipboard: status.settings ? status.settings.clipboard === true : true
  readonly property bool audio: status.settings ? status.settings.audio !== false : true
  readonly property bool ripple: status.settings ? status.settings.ripple !== false : true
  readonly property bool updates: status.settings ? status.settings.updates !== false : true

  // Pairing from here: the computer asked to show a code, and what went wrong if anything.
  property string pairingWith: ""
  property string pairingError: ""
  // Typing a pairing code: the panel's own keys stand aside.
  property bool editing: false
  // The last thing an action had to say (an update check, a failed command).
  property string message: ""
  // Starts with the desktop (the systemd user service is enabled).
  property bool atLogin: true
  // The Arrange Displays overlay is showing.
  property bool arranging: false

  readonly property string summary: {
    if (!running) return "Not running"
    if (pairingCode) return "Pairing with " + (pairingCode.name || "another computer")
    if (controlledBy !== "") return "In use from " + controlledBy
    if (status.controlling) return "Using " + nameOf(status.controlling)
    if (connectedPeers.length > 0) return "Connected to " + connectedPeers.map(function(p) { return p.name }).join(", ")
    return "Not connected"
  }

  function nameOf(id) {
    if (!id) return ""
    var all = status.peers || []
    for (var i = 0; i < all.length; i++) if (all[i].id === id) return all[i].name
    return id
  }

  function peerDetail(p) {
    if (!p.paired) return p.connected ? "Found on your network" : "Not paired"
    if (p.paused) return p.connected ? "Paused" : "Paused · Offline"
    if (!p.connected) return "Offline"
    if (status.controlled_by === p.id) return "Using this computer now"
    var parts = ["Connected"]
    if (p.sound === "here") parts.push("its sound plays here")
    if (p.sound === "there") parts.push("plays your sound")
    return parts.join(" · ")
  }

  function apply(line) {
    var data
    try { data = JSON.parse(line) } catch (e) { return }
    var hadCode = !!pairingCode
    running = data.running === true
    status = running ? data : ({})
    // A new pairing code is the one thing worth interrupting for.
    if (!hadCode && pairingCode && !opened) open()
  }

  // Settings changes run one after another; one made while another is still running waits.
  property var queued: []

  function runSetter(command) {
    if (setter.running) {
      queued = queued.concat([command])
      return
    }
    setter.command = command
    setter.running = true
  }

  function setSetting(key, on) {
    runSetter([binary, "set", key, on ? "on" : "off"])
  }

  function setPaused(p, paused) {
    runSetter([binary, paused ? "pause" : "resume", p.id])
  }

  function forget(p) {
    runSetter([binary, "unpair", p.id])
  }

  function setAtLogin(on) {
    atLogin = on
    runSetter(["systemctl", "--user", on ? "enable" : "disable", "mousetail"])
  }

  function startPairing(p) {
    pairer.running = false
    pairingWith = p.id
    pairingError = ""
    pairer.command = [binary, "pair", p.id]
    pairer.running = true
  }

  function sendCode(code) {
    if (code.trim() === "") return
    pairingError = ""
    pairer.write(code.trim() + "\n")
  }

  function cancelPairing() {
    pairer.running = false
    pairingWith = ""
    pairingError = ""
    editing = false
  }

  function checkForUpdates() {
    message = "Checking for updates…"
    updater.running = true
  }

  onOpenedChanged: if (opened) {
    message = ""
    loginCheck.running = true
  }

  Process {
    id: watcher
    command: [root.binary, "watch"]
    running: true
    stdout: SplitParser { onRead: function(line) { root.apply(line) } }
    onExited: {
      root.running = false
      restart.start()
    }
  }

  Timer {
    id: restart
    interval: 3000
    onTriggered: watcher.running = true
  }

  Process {
    id: setter
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.message = text.trim().replace(/^mousetail: /, "")
    }
    onExited: {
      if (root.queued.length > 0) {
        var next = root.queued[0]
        root.queued = root.queued.slice(1)
        root.runSetter(next)
      }
    }
  }

  // `mousetail pair` asks the other computer to show a code, then reads it from stdin.
  Process {
    id: pairer
    stdinEnabled: true
    stdout: SplitParser {
      onRead: function(line) {
        if (line.indexOf("Paired with") >= 0) {
          root.message = line.trim()
          root.pairingWith = ""
          root.editing = false
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.pairingError = text.trim().replace(/^mousetail: /, "")
    }
    onExited: function(code) {
      // A wrong code ends the command: offer to start again rather than leave a dead field.
      if (code !== 0 && root.pairingWith !== "" && root.pairingError === "")
        root.pairingError = "That didn't work. Try again."
    }
  }

  Process {
    id: updater
    command: [root.binary, "update"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.message = text.trim()
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.message = text.trim().replace(/^mousetail: /, "")
    }
  }

  // enable-firewall.sh asks for a password, so it runs in a terminal.
  Process {
    id: firewallFixer
    command: ["bash", "-c",
      "script=\"$HOME/.local/share/mousetail/enable-firewall.sh\"; " +
      "if command -v omarchy-launch-floating-terminal-with-presentation >/dev/null; then " +
      "exec omarchy-launch-floating-terminal-with-presentation \"$script\"; fi; " +
      "exec xdg-terminal-exec bash -c \"$script; read -rp 'Press Enter to close. '\""]
  }

  Process {
    id: loginCheck
    command: ["systemctl", "--user", "is-enabled", "mousetail"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.atLogin = text.trim() === "enabled"
    }
  }

  // `quickshell ipc -p /usr/share/omarchy/shell call nz.galengreen.mousetail.arrange open`,
  // for a key binding.
  IpcHandler {
    target: "nz.galengreen.mousetail.arrange"
    function open(): void { root.arranging = true }
    function close(): void { root.arranging = false }
  }

  ArrangeWindow {
    visible: root.arranging
    binary: root.binary
    fontFamily: root.fontFamily
    onDone: root.arranging = false
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component { LogoIcon {} }
    tooltipText: "MouseTail: " + root.summary
    onPressed: function(b) { root.toggle() }
  }

  // The MouseTail icon (icon.svg, made from the Mac's menu bar icon), tinted like the bar's text.
  component LogoIcon: Item {
    Image {
      id: logoImage
      anchors.centerIn: parent
      width: parent.width * 1.3
      height: parent.height
      fillMode: Image.PreserveAspectFit
      source: Qt.resolvedUrl("icon.svg")
      sourceSize.width: Math.round(width * Screen.devicePixelRatio)
      sourceSize.height: Math.round(height * Screen.devicePixelRatio)
      // Kept as a hidden layer so the effect can sample it as a texture.
      visible: false
      layer.enabled: true
    }

    MultiEffect {
      anchors.fill: logoImage
      source: logoImage
      brightness: 1.0
      colorization: 1.0
      colorizationColor: root.foreground
    }
  }

  component SettingRow: Item {
    property string label: ""
    property bool checked: false
    signal toggled()

    width: parent ? parent.width : 0
    implicitHeight: Math.max(rowLabel.implicitHeight, rowSwitch.implicitHeight)

    Text {
      id: rowLabel
      text: parent.label
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
    }

    ToggleSwitch {
      id: rowSwitch
      checked: parent.checked
      foreground: root.foreground
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      onToggled: parent.toggled()
    }
  }

  component Note: Text {
    width: parent ? parent.width : 0
    wrapMode: Text.WordWrap
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(360))
    contentHeight: panel.fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      // While a pairing code is being typed, the keys are the field's.
      blocked: root.editing
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Column {
        id: column
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(18)

        PanelHero {
          width: parent.width
          title: "MouseTail"
          meta: root.summary
          foreground: root.foreground
          fontFamily: root.fontFamily
          iconComponent: Component {
            LogoIcon {
              implicitWidth: Style.font.display * 1.5
              implicitHeight: Style.font.display
            }
          }
        }

        // Anything stopping MouseTail doing its job here.
        Column {
          width: parent.width
          spacing: Style.space(4)
          visible: root.running && (!!root.status.capture_error || root.status.can_be_controlled === false || !!root.status.firewall)

          Note {
            visible: !!root.status.capture_error
            text: root.status.capture_error || ""
            color: Color.urgent
          }

          Note {
            visible: root.status.can_be_controlled === false
            text: "Other computers can't control this one yet. Run this once: ~/.local/share/mousetail/enable-input.sh"
          }

          Note {
            visible: !!root.status.firewall
            text: "This computer's firewall stops other computers reaching it, so connecting can be slow or fail."
          }

          Button {
            visible: !!root.status.firewall
            text: "Fix Firewall…"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            enabled: !firewallFixer.running
            onClicked: firewallFixer.running = true
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(6)
          visible: root.pairingCode !== null

          PanelSectionHeader {
            width: parent.width
            text: "Pairing code"
          }

          Text {
            text: root.pairingCode ? root.pairingCode.code.split("").join(" ") : ""
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.display
            font.weight: Font.DemiBold
          }

          Note {
            text: "Type this on " + (root.pairingCode && root.pairingCode.name ? root.pairingCode.name : "your other computer") + " to connect it."
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(10)
          visible: root.running

          PanelSectionHeader {
            width: parent.width
            text: "Computers"
          }

          Note {
            visible: root.peers.length === 0
            text: "Looking for other computers on your network…"
            font.pixelSize: Style.font.bodySmall
          }

          Repeater {
            model: root.peers

            delegate: Column {
              id: peerRow
              required property var modelData
              readonly property bool pairing: root.pairingWith === modelData.id
              width: parent.width
              spacing: Style.space(6)

              Item {
                width: parent.width
                implicitHeight: Math.max(peerText.implicitHeight, actions.implicitHeight)

                Column {
                  id: peerText
                  anchors.left: parent.left
                  anchors.right: actions.left
                  anchors.rightMargin: Style.space(8)
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(1)

                  Text {
                    width: parent.width
                    elide: Text.ElideRight
                    text: peerRow.modelData.name
                    color: peerRow.modelData.connected && !peerRow.modelData.paused ? root.foreground : root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                  }

                  Text {
                    width: parent.width
                    elide: Text.ElideRight
                    text: root.peerDetail(peerRow.modelData)
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                  }
                }

                Row {
                  id: actions
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(2)

                  Button {
                    visible: !peerRow.modelData.paired && peerRow.modelData.connected && !peerRow.pairing
                    text: "Pair…"
                    bordered: true
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    fontSize: Style.font.bodySmall
                    onClicked: root.startPairing(peerRow.modelData)
                  }

                  PanelActionButton {
                    visible: peerRow.modelData.paired
                    iconText: peerRow.modelData.paused ? "󰐊" : "󰏤"
                    tooltipText: peerRow.modelData.paused ? "Resume " + peerRow.modelData.name : "Pause " + peerRow.modelData.name + " without forgetting it"
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    onClicked: root.setPaused(peerRow.modelData, !peerRow.modelData.paused)
                  }

                  PanelActionButton {
                    visible: peerRow.modelData.paired
                    iconText: "󰅙"
                    tooltipText: "Forget " + peerRow.modelData.name
                    foreground: root.foreground
                    hoverColor: Color.urgent
                    fontFamily: root.fontFamily
                    onClicked: root.forget(peerRow.modelData)
                  }
                }
              }

              // Pairing: the other computer is showing a code to type here.
              Column {
                visible: peerRow.pairing
                width: parent.width
                spacing: Style.space(6)

                Note {
                  text: "Type the code showing on " + peerRow.modelData.name + ":"
                }

                Item {
                  width: parent.width
                  implicitHeight: codeField.implicitHeight

                  TextField {
                    id: codeField
                    anchors.left: parent.left
                    anchors.right: cancelPair.left
                    anchors.rightMargin: Style.space(6)
                    placeholderText: "Code"
                    inputMethodHints: Qt.ImhDigitsOnly
                    maximumLength: 8
                    foreground: root.foreground
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                    onActiveFocusChanged: root.editing = activeFocus
                    onAccepted: {
                      root.sendCode(text)
                      text = ""
                    }
                    Keys.onPressed: function(event) {
                      if (event.key === Qt.Key_Escape) {
                        root.cancelPairing()
                        event.accepted = true
                      }
                    }
                  }

                  Button {
                    id: cancelPair
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Cancel"
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    fontSize: Style.font.bodySmall
                    onClicked: root.cancelPairing()
                  }
                }

                Note {
                  visible: root.pairingError !== ""
                  text: root.pairingError
                  color: Color.urgent
                }

                // Focus the field as soon as it shows, so the code can just be typed.
                onVisibleChanged: if (visible) codeField.forceActiveFocus()
              }
            }
          }

          Button {
            visible: root.peers.some(function(p) { return p.paired })
            text: "Arrange Displays…"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: {
              root.close()
              root.arranging = true
            }
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(6)
          visible: root.running

          PanelSectionHeader {
            width: parent.width
            text: "Settings"
          }

          SettingRow {
            label: "Sound follows you"
            checked: root.audio
            onToggled: root.setSetting("audio", !root.audio)
          }

          SettingRow {
            label: "Share clipboard"
            checked: root.clipboard
            onToggled: root.setSetting("clipboard", !root.clipboard)
          }

          SettingRow {
            label: "Ripple when crossing"
            checked: root.ripple
            onToggled: root.setSetting("ripple", !root.ripple)
          }

          SettingRow {
            label: "Start at login"
            checked: root.atLogin
            onToggled: root.setAtLogin(!root.atLogin)
          }

          SettingRow {
            label: "Update automatically"
            checked: root.updates
            onToggled: root.setSetting("updates", !root.updates)
          }
        }

        Note {
          visible: !root.running
          text: "MouseTail isn't running."
          font.pixelSize: Style.font.bodySmall
        }

        Column {
          width: parent.width
          spacing: Style.space(8)

          Note {
            visible: root.message !== ""
            text: root.message
          }

          Row {
            spacing: Style.space(6)

            Button {
              visible: root.running
              text: "Check for Updates"
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              enabled: !updater.running
              onClicked: root.checkForUpdates()
            }

            Button {
              text: root.running ? "Stop MouseTail" : "Start MouseTail"
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              onClicked: root.runSetter(["systemctl", "--user", root.running ? "stop" : "start", "mousetail"])
            }
          }

          Note {
            visible: !!root.status.version
            text: "MouseTail " + (root.status.version || "")
          }
        }
      }
    }
  }
}
