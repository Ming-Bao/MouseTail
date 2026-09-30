import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import qs.Ui

// Drag other computers to where they sit on your desk, like the Mac's Arrange Displays, in
// MouseTail's own colours (as the website and the Mac window use) rather than the theme's.
// Drops snap to the nearest edge; highlighted edges are where the cursor crosses. An overlay
// rather than a window, as Omarchy's own launcher and menus are, so it floats over whatever's
// open without any window rules.
PanelWindow {
  id: win

  required property string binary
  property string fontFamily: Style.font.family

  // MouseTail's colours: near-black, warm off-white, and the logo's glowing yellow tail for
  // where the cursor crosses. Tiles: graphite for this computer, warm for the others.
  readonly property color text: "#f2f1ec"
  readonly property color muted: Qt.rgba(0.949, 0.945, 0.925, 0.64)
  readonly property color faint: Qt.rgba(0.949, 0.945, 0.925, 0.42)
  readonly property color line: Qt.rgba(0.949, 0.945, 0.925, 0.09)
  readonly property color card: "#111212"
  readonly property color glow: "#ffeba7"
  readonly property color glowSoft: Qt.rgba(1, 0.878, 0.431, 0.4)
  readonly property var tiles: ({
    this: { top: "#383939", bottom: "#292a2a", border: 0.12, label: muted },
    other: { top: "#4a4535", bottom: "#353226", border: 0.18, label: text },
    offline: { top: "#232424", bottom: "#1b1c1c", border: 0.08, label: faint }
  })

  signal done()

  // `mousetail layout`: every machine's displays and offset, and the crossing edges.
  property var layout: null
  // Machine being dragged, and how far (view pixels).
  property string dragging: ""
  property point dragDelta: Qt.point(0, 0)
  // The view transform is frozen during a drag so the canvas doesn't rescale under you.
  property var frozen: null

  readonly property var shown: layout ? layout.machines.filter(function(m) { return m.this || m.offset }) : []
  readonly property var view: frozen || fit(canvas.width, canvas.height)

  anchors { top: true; bottom: true; left: true; right: true }
  exclusionMode: ExclusionMode.Ignore
  color: Qt.rgba(0, 0, 0, 0.45)
  WlrLayershell.layer: WlrLayer.Overlay
  WlrLayershell.namespace: "mousetail-arrange"
  WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive

  function offsetOf(m) { return m.offset || { x: 0, y: 0 } }

  // Maps layout points to view points, fitting everything with a margin.
  function fit(width, height) {
    var rects = []
    shown.forEach(function(m) {
      var o = offsetOf(m)
      m.displays.forEach(function(d) { rects.push({ x: d.rect.x + o.x, y: d.rect.y + o.y, w: d.rect.w, h: d.rect.h }) })
    })
    if (rects.length === 0 || width <= 0 || height <= 0) return { scale: 0.1, x: 0, y: 0, bx: 0, by: 0 }
    var x0 = Math.min.apply(null, rects.map(function(r) { return r.x }))
    var y0 = Math.min.apply(null, rects.map(function(r) { return r.y }))
    var x1 = Math.max.apply(null, rects.map(function(r) { return r.x + r.w }))
    var y1 = Math.max.apply(null, rects.map(function(r) { return r.y + r.h }))
    var margin = 40
    var scale = Math.max(0.01, Math.min((width - 2 * margin) / (x1 - x0), (height - 2 * margin) / (y1 - y0)))
    return { scale: scale, x: (width - (x1 - x0) * scale) / 2, y: (height - (y1 - y0) * scale) / 2, bx: x0, by: y0 }
  }

  function viewX(x) { return view.x + (x - view.bx) * view.scale }
  function viewY(y) { return view.y + (y - view.by) * view.scale }

  function refresh() {
    if (!loader.running) loader.running = true
  }

  function drop(m) {
    var o = offsetOf(m)
    var x = o.x + dragDelta.x / view.scale
    var y = o.y + dragDelta.y / view.scale
    placer.command = [binary, "place-at", m.id, String(Math.round(x)), String(Math.round(y))]
    placer.running = true
  }

  Process {
    id: loader
    command: [win.binary, "layout"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          var v = JSON.parse(text)
          if (v.machines) win.layout = v
        } catch (e) {}
      }
    }
  }

  // Keep up with the other computer being moved from there, or connecting.
  Timer {
    interval: 2000
    repeat: true
    running: win.visible && win.dragging === ""
    triggeredOnStart: true
    onTriggered: win.refresh()
  }

  // The layout arrives already moved to where it snapped; the tile glides there.
  Process {
    id: placer
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          var v = JSON.parse(text)
          if (v.offset && win.layout) {
            var id = win.dragging
            win.layout = {
              crossings: [],
              machines: win.layout.machines.map(function(m) {
                if (m.id !== id) return m
                var moved = Object.assign({}, m)
                moved.offset = v.offset
                return moved
              })
            }
          }
        } catch (e) {}
        win.dragging = ""
        win.dragDelta = Qt.point(0, 0)
        win.frozen = null
        win.refresh()
      }
    }
  }

  // Click outside the card to close.
  MouseArea {
    anchors.fill: parent
    onClicked: win.done()
  }

  Rectangle {
    id: card
    anchors.centerIn: parent
    width: Math.min(parent.width * 0.7, 900)
    height: Math.min(parent.height * 0.7, 620)
    color: "#080808"
    border.color: win.line
    border.width: 1
    radius: Style.cornerRadius

    // Clicks on the card stay on the card.
    MouseArea { anchors.fill: parent }

    FocusScope {
      anchors.fill: parent
      anchors.margins: Style.space(20)
      focus: true
      Keys.onEscapePressed: win.done()

      Column {
        id: heading
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(6)

        Text {
          text: "Arrange Displays"
          color: win.text
          font.family: win.fontFamily
          font.pixelSize: Style.font.title
          font.weight: Font.DemiBold
        }

        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          text: "Drag each computer to where it sits on your desk. Push the cursor off a highlighted edge to move to the other computer."
          color: win.muted
          font.family: win.fontFamily
          font.pixelSize: Style.font.bodySmall
        }
      }

      Rectangle {
        id: canvasFrame
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: heading.bottom
        anchors.topMargin: Style.space(14)
        anchors.bottom: footer.top
        anchors.bottomMargin: Style.space(14)
        radius: Style.cornerRadius
        color: win.card
        border.color: win.line
        border.width: 1
        clip: true

        Item {
          id: canvas
          anchors.fill: parent

          Text {
            anchors.centerIn: parent
            visible: win.layout === null
            text: "Loading…"
            color: win.muted
            font.family: win.fontFamily
            font.pixelSize: Style.font.bodySmall
          }

          Repeater {
            model: win.shown

            delegate: Item {
              id: machineItem
              required property var modelData
              readonly property bool lifted: win.dragging === modelData.id
              readonly property var style: modelData.this ? win.tiles.this
                : (modelData.connected && !modelData.paused ? win.tiles.other : win.tiles.offline)
              x: win.viewX(win.offsetOf(modelData).x) + (lifted ? win.dragDelta.x : 0)
              y: win.viewY(win.offsetOf(modelData).y) + (lifted ? win.dragDelta.y : 0)
              z: lifted ? 2 : 1

              Behavior on x { enabled: !machineItem.lifted; NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
              Behavior on y { enabled: !machineItem.lifted; NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }

              Repeater {
                model: machineItem.modelData.displays

                delegate: Rectangle {
                  id: tile
                  required property var modelData
                  x: modelData.rect.x * win.view.scale
                  y: modelData.rect.y * win.view.scale
                  width: modelData.rect.w * win.view.scale
                  height: modelData.rect.h * win.view.scale
                  radius: 5
                  gradient: Gradient {
                    GradientStop { position: 0; color: machineItem.style.top }
                    GradientStop { position: 1; color: machineItem.style.bottom }
                  }
                  border.color: machineItem.lifted ? win.glow : Qt.rgba(1, 1, 1, machineItem.style.border)
                  border.width: machineItem.lifted ? 1.5 : 1

                  // Picked up: it glows, like the logo's tail.
                  Rectangle {
                    visible: machineItem.lifted
                    anchors.fill: parent
                    anchors.margins: -5
                    z: -1
                    radius: 9
                    color: "transparent"
                    border.color: win.glowSoft
                    border.width: 4
                  }

                  // A strip marks each computer's main display, as macOS does.
                  Rectangle {
                    visible: tile.modelData.primary
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: 1
                    height: 4
                    radius: 4
                    color: Qt.rgba(0.949, 0.945, 0.925, 0.45)
                  }

                  Column {
                    anchors.centerIn: parent
                    width: parent.width - 12
                    spacing: 2

                    Text {
                      width: parent.width
                      horizontalAlignment: Text.AlignHCenter
                      elide: Text.ElideRight
                      text: machineItem.modelData.this ? "This computer" : machineItem.modelData.name
                      color: machineItem.style.label
                      font.family: win.fontFamily
                      font.pixelSize: Style.font.bodySmall
                      font.weight: Font.DemiBold
                    }

                    Text {
                      width: parent.width
                      horizontalAlignment: Text.AlignHCenter
                      elide: Text.ElideRight
                      text: machineItem.modelData.connected || machineItem.modelData.this ? tile.modelData.name : "Offline"
                      color: machineItem.style.label
                      opacity: 0.85
                      font.family: win.fontFamily
                      font.pixelSize: Style.font.caption
                    }
                  }

                  MouseArea {
                    anchors.fill: parent
                    enabled: !machineItem.modelData.this
                    cursorShape: machineItem.lifted ? Qt.ClosedHandCursor : Qt.OpenHandCursor
                    property point pressedAt
                    onPressed: function(mouse) {
                      pressedAt = mapToItem(canvas, mouse.x, mouse.y)
                    }
                    onPositionChanged: function(mouse) {
                      var p = mapToItem(canvas, mouse.x, mouse.y)
                      if (win.dragging === "") {
                        if (Math.abs(p.x - pressedAt.x) + Math.abs(p.y - pressedAt.y) < 3) return
                        win.frozen = win.view
                        win.dragging = machineItem.modelData.id
                      }
                      win.dragDelta = Qt.point(p.x - pressedAt.x, p.y - pressedAt.y)
                    }
                    onReleased: if (win.dragging === machineItem.modelData.id) win.drop(machineItem.modelData)
                  }
                }
              }
            }
          }

          // Where the cursor crosses (hidden mid-drag, when they'd be out of date).
          Repeater {
            model: win.dragging === "" && win.layout ? (win.layout.crossings || []) : []

            delegate: Rectangle {
              required property var modelData
              readonly property real x0: win.viewX(Math.min(modelData[0].x, modelData[1].x))
              readonly property real y0: win.viewY(Math.min(modelData[0].y, modelData[1].y))
              readonly property real x1: win.viewX(Math.max(modelData[0].x, modelData[1].x))
              readonly property real y1: win.viewY(Math.max(modelData[0].y, modelData[1].y))
              x: x0 - 1.5
              y: y0 - 1.5
              width: x1 - x0 + 3
              height: y1 - y0 + 3
              radius: 1.5
              z: 3
              color: win.glow

              // Its glow.
              Rectangle {
                anchors.fill: parent
                anchors.margins: -4
                z: -1
                radius: 6
                color: win.glowSoft
                opacity: 0.6
              }
            }
          }
        }
      }

      Item {
        id: footer
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        implicitHeight: doneButton.implicitHeight

        Row {
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.space(14)

          Repeater {
            model: [
              { label: "This computer", colour: win.tiles.this.top, line: false },
              { label: "Other computers", colour: win.tiles.other.top, line: false },
              { label: "Cursor crosses here", colour: win.glow, line: true }
            ]

            delegate: Row {
              required property var modelData
              spacing: Style.space(5)

              Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: 14
                height: parent.modelData.line ? 3 : 10
                radius: 2
                color: parent.modelData.colour
                border.color: parent.modelData.line ? "transparent" : Qt.rgba(1, 1, 1, 0.15)
                border.width: 1
              }

              Text {
                text: parent.modelData.label
                color: win.muted
                font.family: win.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }
        }

        Button {
          id: doneButton
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          text: "Done"
          foreground: "#080808"
          background: win.glow
          fontFamily: win.fontFamily
          fontSize: Style.font.bodySmall
          onClicked: win.done()
        }
      }
    }
  }
}
