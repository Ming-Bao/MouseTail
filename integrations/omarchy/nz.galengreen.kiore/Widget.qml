import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Kiore in the Omarchy bar: a mouse that lights up while another computer is connected,
// and a panel showing who's connected, any pairing code, and the clipboard setting. Status
// streams from `kiore watch`, one JSON line per change.
Panel {
  id: root
  moduleName: "nz.galengreen.kiore"
  ipcTarget: "nz.galengreen.kiore"

  readonly property string binary: Quickshell.env("HOME") + "/.local/bin/kiore"
  readonly property string glyph: "󰍽"

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  property bool running: false
  property var status: ({})

  readonly property var peers: (status.peers || []).filter(function(p) { return p.paired || p.connected })
  readonly property var connectedPeers: peers.filter(function(p) { return p.paired && p.connected })
  readonly property string controlledBy: nameOf(status.controlled_by)
  readonly property var pairingCode: status.pairing_code || null
  readonly property bool clipboard: status.settings ? status.settings.clipboard === true : true
  readonly property bool audio: status.settings ? status.settings.audio !== false : true
  readonly property string macName: connectedPeers.length > 0 ? connectedPeers[0].name : "your Mac"
  readonly property bool lit: running && connectedPeers.length > 0

  readonly property string summary: {
    if (!running) return "Not running"
    if (pairingCode) return "Pairing with " + (pairingCode.name || "another computer")
    if (controlledBy !== "") return "In use from " + controlledBy
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
    if (!p.paired) return "Found on your network. Pair it from your Mac."
    if (!p.connected) return "Offline"
    if (status.controlled_by === p.id) return "Using this computer now"
    return "Connected"
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

  function setClipboard(on) {
    setter.command = [binary, "set", "clipboard", on ? "on" : "off"]
    setter.running = true
  }

  function setAudio(on) {
    setter.command = [binary, "set", "audio", on ? "on" : "off"]
    setter.running = true
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

  Process { id: setter }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.glyph
    opacity: root.lit ? 1.0 : 0.45
    tooltipText: "Kiore: " + root.summary
    onPressed: function(b) { root.toggle() }
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

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(340))
    contentHeight: panel.fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
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
          title: "Kiore"
          meta: root.summary
          foreground: root.foreground
          fontFamily: root.fontFamily
          iconOpacity: root.lit ? 1.0 : 0.5
          iconComponent: Component {
            Text {
              text: root.glyph
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.display
            }
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

          Text {
            width: parent.width
            wrapMode: Text.WordWrap
            text: "Type this on " + (root.pairingCode && root.pairingCode.name ? root.pairingCode.name : "your other computer") + " to connect it."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(6)
          visible: root.running

          PanelSectionHeader {
            width: parent.width
            text: "Computers"
          }

          Text {
            visible: root.peers.length === 0
            width: parent.width
            wrapMode: Text.WordWrap
            text: "Looking for other computers on your network…"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }

          Repeater {
            model: root.peers

            delegate: Column {
              required property var modelData
              width: parent.width
              spacing: Style.space(1)

              Text {
                text: modelData.name
                color: modelData.connected ? root.foreground : root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }

              Text {
                text: root.peerDetail(modelData)
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
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
            label: "Play sound on " + root.macName
            checked: root.audio
            onToggled: root.setAudio(!root.audio)
          }

          SettingRow {
            label: "Share clipboard"
            checked: root.clipboard
            onToggled: root.setClipboard(!root.clipboard)
          }
        }

        Text {
          visible: !root.running
          width: parent.width
          wrapMode: Text.WordWrap
          text: "Kiore isn't running. Start it with: systemctl --user start kiore"
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }
      }
    }
  }
}
