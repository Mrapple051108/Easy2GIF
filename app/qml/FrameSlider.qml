import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// A horizontal progress bar carrying one draggable vertical line, plus a text
// field that accepts a time point typed straight in.
//
// The component is deliberately dumb: it reports a fraction while dragging and
// a string when the field is committed, and it only ever displays the time it
// is handed back. All authority over the selection lives in the kernel.
Item {
    id: root

    property string label: ""
    property color accent: "#4fc3f7"
    /// Handle position, 0..1, as reported by the kernel.
    property real fraction: 0
    /// Authoritative time text, e.g. "00:02.500".
    property string timeText: "00:00.000"
    /// Authoritative frame index text.
    property string frameText: "0"
    property string hint: ""

    /// Emitted continuously while the line is dragged.
    signal moved(real fraction)
    /// Emitted when the user finishes typing a time and presses Enter.
    signal timeEntered(string text)
    /// Emitted for arrow-key nudges, in fractions of the clip.
    signal nudged(real deltaFraction)

    implicitHeight: 68

    // Keep the field in step with the kernel, but never while the user is
    // typing into it.
    function syncField() {
        if (!field.activeFocus) {
            field.text = root.timeText
        }
    }

    onTimeTextChanged: syncField()
    Component.onCompleted: syncField()

    ColumnLayout {
        anchors.fill: parent
        spacing: 5

        RowLayout {
            Layout.fillWidth: true
            spacing: 10

            Label {
                text: root.label
                color: "#c7d5e3"
                font.pixelSize: 13
                font.bold: true
            }

            Rectangle {
                Layout.preferredHeight: 20
                Layout.preferredWidth: timeLabel.implicitWidth + 16
                radius: 5
                color: "#16222e"
                border.color: root.accent
                border.width: 1

                Label {
                    id: timeLabel
                    anchors.centerIn: parent
                    text: root.timeText
                    color: root.accent
                    font.pixelSize: 13
                    font.family: "Consolas"
                }
            }

            Label {
                text: "帧 " + root.frameText
                color: "#7d8fa3"
                font.pixelSize: 12
            }

            Item { Layout.fillWidth: true }

            Label {
                visible: root.hint.length > 0
                text: root.hint
                color: "#5b6b7c"
                font.pixelSize: 11
            }

            TextField {
                id: field
                Layout.preferredWidth: 132
                placeholderText: "00:00.000"
                horizontalAlignment: TextInput.AlignHCenter
                selectByMouse: true
                font.pixelSize: 12
                color: "#e6eef6"
                placeholderTextColor: "#54636f"

                background: Rectangle {
                    radius: 5
                    color: field.activeFocus ? "#1b2a38" : "#141d27"
                    border.color: field.activeFocus ? root.accent : "#2b3a49"
                    border.width: 1
                }

                onEditingFinished: root.timeEntered(text)
                Keys.onEscapePressed: {
                    field.text = root.timeText
                    focus = false
                }
            }
        }

        // The bar itself.
        Item {
            id: track
            Layout.fillWidth: true
            Layout.preferredHeight: 30

            function fractionAt(px) {
                return Math.max(0, Math.min(1, px / Math.max(1, track.width)))
            }

            Rectangle {
                id: groove
                anchors.verticalCenter: parent.verticalCenter
                width: parent.width
                height: 8
                radius: 4
                color: "#131d27"
                border.color: "#26333f"
                border.width: 1

                // Everything left of the line reads as "inside the clip".
                Rectangle {
                    width: Math.max(0, Math.round(groove.width * root.fraction))
                    height: parent.height
                    radius: parent.radius
                    color: root.accent
                    opacity: 0.5
                }
            }

            // The draggable vertical line.
            Rectangle {
                id: handle
                x: Math.round(root.fraction * track.width) - width / 2
                width: 3
                height: track.height
                radius: 1.5
                color: root.accent

                Rectangle {
                    width: 15
                    height: 22
                    radius: 4
                    color: root.accent
                    border.color: "#0b1219"
                    border.width: 1
                    anchors.horizontalCenter: parent.horizontalCenter
                    anchors.verticalCenter: parent.verticalCenter
                }
            }

            MouseArea {
                id: mouse
                anchors.fill: parent
                cursorShape: Qt.SizeHorCursor
                hoverEnabled: true
                focus: true

                onPressed: root.moved(track.fractionAt(mouse.x))
                onPositionChanged: if (pressed) root.moved(track.fractionAt(mouse.x))

                Keys.onLeftPressed: root.nudged(-0.005)
                Keys.onRightPressed: root.nudged(0.005)
            }
        }
    }
}
