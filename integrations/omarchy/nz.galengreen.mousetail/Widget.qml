import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// MouseTail in the Omarchy bar: the MouseTail icon, and a panel showing who's connected, any pairing code, and the clipboard setting. Status
// streams from `mousetail watch`, one JSON line per change.
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
    if (!p.paired) return "Found on your network. Run mousetail pair to connect it."
    if (p.paused) return p.connected ? "Paused" : "Paused · Offline"
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

  function setClipboard(on) {
    runSetter([binary, "set", "clipboard", on ? "on" : "off"])
  }

  function setRipple(on) {
    runSetter([binary, "set", "ripple", on ? "on" : "off"])
  }

  function setAudio(on) {
    runSetter([binary, "set", "audio", on ? "on" : "off"])
  }

  function setPaused(p, paused) {
    runSetter([binary, paused ? "pause" : "resume", p.id])
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
    onExited: {
      if (root.queued.length > 0) {
        var next = root.queued[0]
        root.queued = root.queued.slice(1)
        root.runSetter(next)
      }
    }
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

            delegate: Item {
              required property var modelData
              width: parent.width
              implicitHeight: peerText.implicitHeight

              Column {
                id: peerText
                anchors.left: parent.left
                anchors.right: pauseButton.left
                spacing: Style.space(1)

                Text {
                  text: modelData.name
                  color: modelData.connected && !modelData.paused ? root.foreground : root.dim
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

              // Pause without forgetting it, and resume.
              Text {
                id: pauseButton
                visible: modelData.paired
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: modelData.paused ? "Resume" : "Pause"
                color: pauseArea.containsMouse ? root.foreground : root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall

                MouseArea {
                  id: pauseArea
                  anchors.fill: parent
                  anchors.margins: -Style.space(4)
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  onClicked: root.setPaused(modelData, !modelData.paused)
                }
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
            label: "Sound follows you"
            checked: root.audio
            onToggled: root.setAudio(!root.audio)
          }

          SettingRow {
            label: "Share clipboard"
            checked: root.clipboard
            onToggled: root.setClipboard(!root.clipboard)
          }

          SettingRow {
            label: "Ripple when crossing"
            checked: root.ripple
            onToggled: root.setRipple(!root.ripple)
          }
        }

        Text {
          visible: !root.running
          width: parent.width
          wrapMode: Text.WordWrap
          text: "MouseTail isn't running. Start it with: systemctl --user start mousetail"
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }
      }
    }
  }
}
