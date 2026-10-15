import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: root
    required property var theme
    property string label: ""
    property color accent: theme.accent
    property real fraction: 0
    property string timeText: "00:00.000"
    property string frameText: "0"
    property string hint: ""
    signal moved(real fraction)
    signal timeEntered(string text)
    signal nudged(real deltaFraction)

    implicitHeight: 40
    opacity: enabled ? 1 : 0.5

    function syncField() {
        if (!field.activeFocus)
            field.text = root.timeText;
    }
    onTimeTextChanged: syncField()
    Component.onCompleted: syncField()

    RowLayout {
        anchors.fill: parent
        spacing: 12
        Label {
            font.family: root.theme.font.family
            text: root.label
            color: root.theme.textDim
            font.pixelSize: 12
            Layout.preferredWidth: 32
        }
        Slider {
            id: seek
            Layout.fillWidth: true
            from: 0
            to: 1
            stepSize: 0.001
            implicitHeight: 32
            leftPadding: 8
            rightPadding: 8
            Accessible.name: root.label
            Binding {
                target: seek
                property: "value"
                value: root.fraction
                restoreMode: Binding.RestoreNone
            }
            onMoved: root.moved(value)
            Keys.onLeftPressed: root.nudged(-0.005)
            Keys.onRightPressed: root.nudged(0.005)
            background: Rectangle {
                x: seek.leftPadding
                y: (seek.height - height) / 2
                width: seek.availableWidth
                height: 4
                radius: 2
                color: root.theme.trackBg
                Rectangle {
                    width: seek.visualPosition * parent.width
                    height: 4
                    radius: 2
                    color: root.accent
                    opacity: 0.65
                }
            }
            handle: Rectangle {
                x: seek.leftPadding + seek.visualPosition * (seek.availableWidth - width)
                y: (seek.height - height) / 2
                width: 16
                height: 16
                radius: 8
                color: root.theme.cardBg
                border.color: root.accent
                border.width: 3
            }
        }
        TextField {
            id: field
            Layout.preferredWidth: 118
            implicitHeight: 34
            horizontalAlignment: TextInput.AlignHCenter
            placeholderText: "00:00.000"
            selectByMouse: true
            font.family: "Consolas"
            font.pixelSize: 12
            color: root.theme.textMain
            placeholderTextColor: root.theme.textMuted
            selectionColor: root.accent
            selectedTextColor: "#ffffff"
            Accessible.name: root.label + qsTr("时间")
            background: Rectangle {
                radius: 9
                color: root.theme.fieldBg
                border.color: root.accent
                border.width: field.activeFocus ? 1 : 0
            }
            ToolTip.visible: hovered
            ToolTip.text: root.hint
            ToolTip.delay: 500
            onEditingFinished: {
                root.timeEntered(text);
                field.text = root.timeText;
            }
            Keys.onEscapePressed: {
                text = root.timeText;
                focus = false;
            }
        }
        Label {
            font.family: root.theme.font.family
            text: qsTr("第 %1 帧").arg(root.frameText)
            color: root.theme.textMuted
            font.pixelSize: 11
            horizontalAlignment: Text.AlignRight
            Layout.preferredWidth: 68
            elide: Text.ElideRight
        }
    }
}
