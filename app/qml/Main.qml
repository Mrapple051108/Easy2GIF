import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs

ApplicationWindow {
    id: window
    width: 1280
    height: 960
    minimumWidth: 1040
    minimumHeight: 720
    visible: true
    title: "Easy2GIF"
    font.family: "Microsoft YaHei UI"
    font.pixelSize: 13
    color: pageBg
    readonly property bool dark: gif.darkMode
    readonly property color pageBg: dark ? "#14161c" : "#f4f5f8"
    readonly property color cardBg: dark ? "#1d2028" : "#ffffff"
    readonly property color cardLine: dark ? "#30343f" : "#e5e7ee"
    readonly property color fieldBg: dark ? "#252934" : "#f5f6fa"
    readonly property color hoverBg: dark ? "#303545" : "#eceef5"
    readonly property color textMain: dark ? "#eff1f7" : "#252a3b"
    readonly property color textDim: dark ? "#a2a9bb" : "#70798d"
    readonly property color textMuted: dark ? "#7f889e" : "#939aab"
    readonly property color accent: dark ? "#a0a5ff" : "#6264dd"
    readonly property color accentSoft: dark ? "#30304b" : "#eeeeff"
    readonly property color primary: "#6264dd"
    readonly property color primaryHover: "#5355ca"
    readonly property color secondary: dark ? "#dbb57f" : "#a77638"
    readonly property color success: dark ? "#8dd5b4" : "#298363"
    readonly property color warning: dark ? "#e4be86" : "#946423"
    readonly property color danger: dark ? "#f0a0a8" : "#bb4657"
    readonly property color dangerBg: dark ? "#38262e" : "#fff0f2"
    readonly property color trackBg: dark ? "#363c4b" : "#e5e8f0"
    palette.window: pageBg
    palette.windowText: textMain
    palette.base: fieldBg
    palette.text: textMain
    palette.button: fieldBg
    palette.buttonText: textMain
    palette.highlight: accent
    palette.highlightedText: "#ffffff"
    palette.placeholderText: textMuted
    palette.mid: cardLine
    function pathToUrl(p) {
        return "file:///" + p.replace(/\\/g, "/");
    }

    component UiLabel: Label {
        font.family: window.font.family
    }

    component Card: Rectangle {
        id: card
        property string title: ""
        property Component tools: null
        default property alias content: inner.data
        implicitHeight: inner.implicitHeight + 32
        radius: 16
        color: window.cardBg
        border.color: window.cardLine
        ColumnLayout {
            id: inner
            anchors.fill: parent
            anchors.margins: 16
            spacing: 10
            RowLayout {
                visible: card.title.length > 0 || card.tools !== null
                Layout.fillWidth: true
                UiLabel {
                    text: card.title
                    color: window.textMain
                    font.pixelSize: 14
                    font.weight: Font.DemiBold
                    Layout.fillWidth: true
                }
                Loader {
                    sourceComponent: card.tools
                }
            }
        }
    }
    component FlatButton: Button {
        id: ctl
        font.family: window.font.family
        property string symbol: ""
        implicitHeight: 36
        implicitWidth: Math.max(76, contentItem.implicitWidth + leftPadding + rightPadding)
        leftPadding: 14
        rightPadding: 14
        topPadding: 8
        bottomPadding: 8
        hoverEnabled: true
        font.pixelSize: 12
        font.weight: Font.Medium
        Accessible.name: text
        contentItem: Item {
            implicitWidth: buttonContents.implicitWidth
            implicitHeight: buttonContents.implicitHeight
            Row {
                id: buttonContents
                anchors.centerIn: parent
                spacing: 7
                Text {
                    visible: ctl.symbol.length > 0
                    text: ctl.symbol
                    font.pixelSize: 16
                    font.family: "Segoe UI Symbol"
                    color: ctl.highlighted && ctl.enabled ? "#ffffff" : window.textDim
                    anchors.verticalCenter: parent.verticalCenter
                }
                Text {
                    text: ctl.text
                    font: ctl.font
                    color: !ctl.enabled ? window.textMuted : ctl.highlighted ? "#ffffff" : window.textMain
                    anchors.verticalCenter: parent.verticalCenter
                }
            }
        }
        background: Rectangle {
            radius: 10
            color: !ctl.enabled ? window.fieldBg : ctl.highlighted ? (ctl.down || ctl.hovered ? window.primaryHover : window.primary) : ctl.down || ctl.hovered ? window.hoverBg : ctl.flat ? "transparent" : window.fieldBg
            border.width: ctl.visualFocus ? 2 : 0
            border.color: window.accent
            Behavior on color {
                ColorAnimation {
                    duration: 100
                }
            }
        }
    }
    component FlatCheck: CheckBox {
        id: ctl
        implicitHeight: 30
        spacing: 8
        leftPadding: 0
        rightPadding: 0
        hoverEnabled: true
        indicator: Rectangle {
            x: ctl.leftPadding
            y: (ctl.height - height) / 2
            width: 18
            height: 18
            radius: 5
            color: ctl.checked ? window.primary : ctl.hovered ? window.hoverBg : window.fieldBg
            border.width: ctl.checked ? 0 : 1
            border.color: ctl.visualFocus ? window.accent : window.cardLine
            Text {
                anchors.centerIn: parent
                text: "✓"
                color: "#ffffff"
                font.family: "Segoe UI Symbol"
                font.pixelSize: 13
                font.bold: true
                visible: ctl.checked
            }
        }
        contentItem: Text {
            text: ctl.text
            font.family: window.font.family
            font.pixelSize: 12
            color: ctl.enabled ? window.textMain : window.textMuted
            leftPadding: ctl.indicator.width + ctl.spacing
            verticalAlignment: Text.AlignVCenter
        }
    }
    component FlatField: TextField {
        id: ctl
        font.family: window.font.family
        implicitHeight: 36
        leftPadding: 12
        rightPadding: 12
        selectByMouse: true
        color: window.textMain
        placeholderTextColor: window.textMuted
        selectionColor: window.accent
        selectedTextColor: "#ffffff"
        font.pixelSize: 12
        background: Rectangle {
            radius: 9
            color: window.fieldBg
            border.width: ctl.activeFocus ? 1 : 0
            border.color: window.accent
        }
    }
    component FlatCombo: ComboBox {
        id: ctl
        font.family: window.font.family
        implicitWidth: 150
        implicitHeight: 36
        leftPadding: 12
        rightPadding: 28
        hoverEnabled: true
        font.pixelSize: 12
        // Measure outside the width binding to avoid TextMetrics feedback loops.
        property real measuredWidth: 0
        function measureEntries() {
            let widest = 0;
            for (let i = 0; i < count; ++i) {
                measure.text = textAt(i);
                widest = Math.max(widest, measure.advanceWidth + 42);
            }
            measuredWidth = widest;
        }
        onCountChanged: Qt.callLater(measureEntries)
        Component.onCompleted: Qt.callLater(measureEntries)
        TextMetrics {
            id: measure
            font: ctl.font
        }
        contentItem: Text {
            text: ctl.displayText
            font: ctl.font
            color: ctl.enabled ? window.textMain : window.textMuted
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
        indicator: Text {
            x: ctl.width - width - 12
            y: (ctl.height - height) / 2
            text: "⌄"
            font.pixelSize: 15
            color: window.textDim
        }
        background: Rectangle {
            radius: 9
            color: ctl.hovered ? window.hoverBg : window.fieldBg
            border.width: ctl.visualFocus ? 1 : 0
            border.color: window.accent
        }
        delegate: ItemDelegate {
            id: item
            required property var modelData
            required property int index
            width: ctl.popup.width - 12
            height: 34
            highlighted: ctl.highlightedIndex === index
            contentItem: Text {
                text: item.modelData
                font: ctl.font
                color: item.index === ctl.currentIndex ? window.accent : window.textMain
                verticalAlignment: Text.AlignVCenter
            }
            background: Rectangle {
                radius: 7
                color: item.highlighted ? window.accentSoft : "transparent"
            }
        }
        popup: Popup {
            y: ctl.height + 6
            width: Math.min(Math.max(ctl.width, ctl.measuredWidth), window.width - 60)
            implicitHeight: Math.min(contentItem.implicitHeight + 12, 280)
            padding: 6
            contentItem: ListView {
                clip: true
                implicitHeight: contentHeight
                model: ctl.popup.visible ? ctl.delegateModel : null
                currentIndex: ctl.highlightedIndex
                boundsBehavior: Flickable.StopAtBounds
                ScrollIndicator.vertical: ScrollIndicator {}
            }
            background: Rectangle {
                radius: 12
                color: window.cardBg
                border.color: window.cardLine
            }
        }
    }
    component OptionGroup: ColumnLayout {
        id: group
        property string label: ""
        default property alias controls: fields.data
        spacing: 6
        Layout.fillWidth: true
        UiLabel {
            text: group.label
            color: window.textDim
            font.pixelSize: 11
        }
        ColumnLayout {
            id: fields
            Layout.fillWidth: true
            spacing: 0
        }
    }
    component FlatSlider: Slider {
        id: ctl
        implicitHeight: 28
        leftPadding: 8
        rightPadding: 8
        background: Rectangle {
            x: ctl.leftPadding
            y: (ctl.height - height) / 2
            width: ctl.availableWidth
            height: 4
            radius: 2
            color: window.trackBg
            Rectangle {
                width: ctl.visualPosition * parent.width
                height: 4
                radius: 2
                color: window.accent
            }
        }
        handle: Rectangle {
            x: ctl.leftPadding + ctl.visualPosition * (ctl.availableWidth - width)
            y: (ctl.height - height) / 2
            width: 16
            height: 16
            radius: 8
            color: window.cardBg
            border.width: 3
            border.color: window.accent
        }
    }
    component AdvSlider: RowLayout {
        id: adv
        property string label: ""
        property string hint: ""
        property real from: 0
        property real to: 100
        property real step: 1
        property real value: 0
        signal moved(real v)
        Layout.fillWidth: true
        spacing: 12
        UiLabel {
            text: adv.label
            color: window.textDim
            font.pixelSize: 12
            Layout.preferredWidth: 74
        }
        FlatSlider {
            id: slider
            Layout.fillWidth: true
            from: adv.from
            to: adv.to
            stepSize: adv.step
            Accessible.name: adv.label
            Binding {
                target: slider
                property: "value"
                value: adv.value
                restoreMode: Binding.RestoreNone
            }
            onMoved: adv.moved(value)
            ToolTip.visible: hovered
            ToolTip.text: adv.hint
        }
        UiLabel {
            text: Math.round(adv.value)
            color: window.textMain
            font.pixelSize: 12
            font.family: "Consolas"
            horizontalAlignment: Text.AlignRight
            Layout.preferredWidth: 32
        }
    }
    FileDialog {
        id: openDialog
        title: qsTr("选择视频")
        nameFilters: ["视频文件 (*.mp4 *.mkv *.mov *.avi *.webm *.m4v *.wmv *.flv *.ts *.mpg *.mpeg *.3gp)", "所有文件 (*)"]
        onAccepted: gif.openVideo(selectedFile.toString())
    }
    FolderDialog {
        id: folderDialog
        title: qsTr("选择保存位置")
        currentFolder: gif.outputDir.length > 0 ? window.pathToUrl(gif.outputDir) : ""
        onAccepted: gif.outputDir = gif.toLocalPath(selectedFolder.toString())
    }
    Shortcut {
        sequences: [StandardKey.Open]
        onActivated: if (!gif.exporting && !gif.importing)
            openDialog.open()
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 22
        spacing: 18
        RowLayout {
            Layout.fillWidth: true
            Layout.preferredHeight: 42
            spacing: 12
            Rectangle {
                width: 34
                height: 34
                radius: 11
                color: window.primary
                Text {
                    anchors.centerIn: parent
                    text: "E"
                    color: "#ffffff"
                    font.pixelSize: 21
                    font.weight: Font.DemiBold
                }
            }
            UiLabel {
                text: "Easy2GIF"
                color: window.textMain
                font.pixelSize: 21
                font.weight: Font.DemiBold
                font.letterSpacing: -0.5
            }
            Item {
                Layout.fillWidth: true
            }
            UiLabel {
                visible: !gif.toolsReady && gif.toolsStatus.length > 0
                text: qsTr("视频引擎不可用")
                color: window.warning
                font.pixelSize: 12
                ToolTip.visible: toolHover.hovered
                ToolTip.text: gif.toolsStatus
                HoverHandler {
                    id: toolHover
                }
            }
            UiLabel {
                visible: gif.sandboxed
                text: qsTr("导出受限")
                color: window.warning
                font.pixelSize: 12
            }
            FlatButton {
                id: themeToggle
                objectName: "themeToggle"
                Layout.preferredWidth: 104
                text: window.dark ? qsTr("深色") : qsTr("浅色")
                symbol: window.dark ? "☾" : "☀"
                Accessible.name: window.dark ? qsTr("切换至浅色模式") : qsTr("切换至深色模式")
                onClicked: gif.darkMode = !gif.darkMode
                ToolTip.visible: hovered
                ToolTip.text: window.dark ? qsTr("切换至浅色模式") : qsTr("切换至深色模式")
                ToolTip.delay: 500
            }
            FlatButton {
                text: qsTr("打开视频")
                symbol: "+"
                highlighted: true
                enabled: !gif.exporting && !gif.importing
                onClicked: openDialog.open()
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 18
            ColumnLayout {
                Layout.preferredWidth: 320
                Layout.minimumWidth: 280
                Layout.maximumWidth: 340
                Layout.fillHeight: true
                spacing: 14
                Card {
                    title: qsTr("预览")
                    Layout.fillWidth: true
                    Layout.preferredHeight: 318
                    Rectangle {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        radius: 11
                        color: gif.mediaLoaded ? "#111318" : window.fieldBg
                        Image {
                            anchors.fill: parent
                            anchors.margins: 3
                            fillMode: Image.PreserveAspectFit
                            source: gif.mediaLoaded && gif.posterRevision > 0 ? "image://gifcore/poster?r=" + gif.posterRevision : ""
                            cache: false
                        }
                        Column {
                            anchors.centerIn: parent
                            spacing: 12
                            visible: !gif.mediaLoaded
                            Rectangle {
                                anchors.horizontalCenter: parent.horizontalCenter
                                width: 52
                                height: 52
                                radius: 16
                                color: window.accentSoft
                                Text {
                                    anchors.centerIn: parent
                                    text: "+"
                                    color: window.accent
                                    font.pixelSize: 30
                                    font.weight: Font.Light
                                }
                            }
                            UiLabel {
                                anchors.horizontalCenter: parent.horizontalCenter
                                text: qsTr("打开视频，开始创作")
                                color: window.textDim
                                font.pixelSize: 12
                            }
                        }
                        MouseArea {
                            anchors.fill: parent
                            enabled: !gif.mediaLoaded && !gif.importing
                            cursorShape: Qt.PointingHandCursor
                            onClicked: openDialog.open()
                        }
                    }
                    UiLabel {
                        visible: gif.mediaLoaded
                        Layout.fillWidth: true
                        text: gif.startTimeText
                        color: window.textDim
                        font.family: "Consolas"
                        font.pixelSize: 12
                        horizontalAlignment: Text.AlignHCenter
                    }
                }
                Card {
                    title: qsTr("视频信息")
                    Layout.fillWidth: true
                    tools: Component {
                        FlatButton {
                            text: qsTr("关闭")
                            implicitHeight: 28
                            implicitWidth: 52
                            flat: true
                            enabled: gif.mediaLoaded && !gif.exporting && !gif.importing
                            onClicked: gif.closeVideo()
                        }
                    }
                    UiLabel {
                        Layout.fillWidth: true
                        text: gif.mediaLoaded ? gif.fileName : qsTr("尚未选择视频")
                        color: window.textMain
                        font.pixelSize: 12
                        font.weight: Font.Medium
                        elide: Text.ElideMiddle
                    }
                    Rectangle {
                        Layout.fillWidth: true
                        implicitHeight: 1
                        color: window.cardLine
                    }
                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: 12
                        rowSpacing: 12
                        UiLabel {
                            text: qsTr("分辨率")
                            color: window.textDim
                            font.pixelSize: 12
                        }
                        UiLabel {
                            text: gif.resolutionText
                            color: window.textMain
                            font.pixelSize: 12
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignRight
                        }
                        UiLabel {
                            text: qsTr("帧率")
                            color: window.textDim
                            font.pixelSize: 12
                        }
                        UiLabel {
                            text: gif.mediaLoaded ? gif.fpsText + " fps" : "—"
                            color: window.textMain
                            font.pixelSize: 12
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignRight
                        }
                        UiLabel {
                            text: qsTr("时长")
                            color: window.textDim
                            font.pixelSize: 12
                        }
                        UiLabel {
                            text: gif.durationText
                            color: window.textMain
                            font.pixelSize: 12
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignRight
                        }
                        UiLabel {
                            text: qsTr("总帧数")
                            color: window.textDim
                            font.pixelSize: 12
                        }
                        UiLabel {
                            text: gif.frameCountText
                            color: window.textMain
                            font.pixelSize: 12
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignRight
                        }
                        UiLabel {
                            text: qsTr("编码")
                            color: window.textDim
                            font.pixelSize: 12
                        }
                        UiLabel {
                            text: gif.mediaLoaded ? gif.videoCodec : "—"
                            color: window.textMain
                            font.pixelSize: 12
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignRight
                        }
                        UiLabel {
                            text: qsTr("音轨")
                            color: window.textDim
                            font.pixelSize: 12
                        }
                        UiLabel {
                            text: gif.mediaLoaded ? (gif.hasAudio ? qsTr("有") : qsTr("无")) : "—"
                            color: window.textMain
                            font.pixelSize: 12
                            Layout.fillWidth: true
                            horizontalAlignment: Text.AlignRight
                        }
                    }
                }
                Item {
                    Layout.fillHeight: true
                }
            }
            ScrollView {
                id: rightScroll
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.minimumWidth: 620
                contentWidth: availableWidth
                clip: true
                ScrollBar.vertical.policy: ScrollBar.AsNeeded
                ColumnLayout {
                    width: rightScroll.availableWidth
                    spacing: 12
                    Card {
                        title: qsTr("剪辑区间")
                        Layout.fillWidth: true
                        tools: Component {
                            RowLayout {
                                spacing: 4
                                FlatButton {
                                    text: qsTr("全选")
                                    implicitHeight: 28
                                    implicitWidth: 56
                                    flat: true
                                    enabled: gif.mediaLoaded && !gif.exporting && !gif.importing
                                    onClicked: gif.selectWholeClip()
                                }
                                FlatButton {
                                    text: qsTr("单帧")
                                    implicitHeight: 28
                                    implicitWidth: 56
                                    flat: true
                                    enabled: gif.mediaLoaded && !gif.exporting && !gif.importing
                                    onClicked: gif.clearSelection()
                                }
                            }
                        }
                        Rectangle {
                            Layout.fillWidth: true
                            Layout.preferredHeight: 76
                            radius: 10
                            color: window.fieldBg
                            Item {
                                anchors.fill: parent
                                anchors.margins: 6
                                Image {
                                    anchors.fill: parent
                                    fillMode: Image.Stretch
                                    cache: false
                                    source: gif.filmstripReady ? "image://gifcore/strip?r=" + gif.filmstripRevision : ""
                                }
                                UiLabel {
                                    anchors.centerIn: parent
                                    visible: !gif.mediaLoaded
                                    text: qsTr("时间轴")
                                    color: window.textMuted
                                    font.pixelSize: 12
                                }
                                Rectangle {
                                    width: Math.round(gif.startFraction * parent.width)
                                    height: parent.height
                                    color: "#151725"
                                    opacity: gif.mediaLoaded ? 0.65 : 0
                                }
                                Rectangle {
                                    x: Math.round(gif.endFraction * parent.width)
                                    width: parent.width - x
                                    height: parent.height
                                    color: "#151725"
                                    opacity: gif.mediaLoaded ? 0.65 : 0
                                }
                                Rectangle {
                                    visible: gif.mediaLoaded
                                    x: Math.round(gif.startFraction * parent.width)
                                    width: Math.max(2, Math.round((gif.endFraction - gif.startFraction) * parent.width))
                                    height: parent.height
                                    color: "transparent"
                                    border.color: window.accent
                                    border.width: 2
                                    radius: 3
                                }
                                Repeater {
                                    model: [gif.startFraction, gif.endFraction]
                                    Rectangle {
                                        required property int index
                                        required property real modelData
                                        visible: gif.mediaLoaded
                                        x: Math.max(0, Math.min(parent.width - width, modelData * parent.width - width / 2))
                                        width: 7
                                        height: parent.height
                                        radius: 3
                                        color: index === 0 ? window.accent : window.secondary
                                        Rectangle {
                                            anchors.centerIn: parent
                                            width: 1
                                            height: 14
                                            radius: 0.5
                                            color: window.cardBg
                                        }
                                    }
                                }
                                MouseArea {
                                    anchors.fill: parent
                                    enabled: gif.mediaLoaded && !gif.exporting && !gif.importing
                                    cursorShape: Qt.SizeHorCursor
                                    property bool grabbingStart: true
                                    function fractionAt(px) {
                                        return Math.max(0, Math.min(1, px / Math.max(1, width)));
                                    }
                                    onPressed: function (mouse) {
                                        const f = fractionAt(mouse.x);
                                        grabbingStart = Math.abs(f - gif.startFraction) <= Math.abs(f - gif.endFraction);
                                        if (grabbingStart)
                                            gif.setStartFraction(f);
                                        else
                                            gif.setEndFraction(f);
                                    }
                                    onPositionChanged: function (mouse) {
                                        if (!pressed)
                                            return;
                                        const f = fractionAt(mouse.x);
                                        if (grabbingStart)
                                            gif.setStartFraction(f);
                                        else
                                            gif.setEndFraction(f);
                                    }
                                }
                            }
                        }
                        FrameSlider {
                            id: startSlider
                            Layout.fillWidth: true
                            theme: window
                            label: qsTr("开始")
                            accent: window.accent
                            hint: qsTr("输入时间或帧号，例如 00:01.500、1:02、150f")
                            fraction: gif.startFraction
                            timeText: gif.startTimeText
                            frameText: gif.startFrameText
                            enabled: gif.mediaLoaded && !gif.exporting && !gif.importing
                            onMoved: function (f) {
                                gif.setStartFraction(f);
                            }
                            onTimeEntered: function (t) {
                                const parsed = gif.parseTime(t);
                                if (parsed >= 0)
                                    gif.setStartTime(parsed);
                                startSlider.syncField();
                            }
                            onNudged: function (d) {
                                gif.nudgeStart(d * gif.duration);
                            }
                        }
                        FrameSlider {
                            id: endSlider
                            Layout.fillWidth: true
                            theme: window
                            label: qsTr("结束")
                            accent: window.secondary
                            hint: qsTr("输入结束时间或帧号")
                            fraction: gif.endFraction
                            timeText: gif.endTimeText
                            frameText: gif.endFrameText
                            enabled: gif.mediaLoaded && !gif.exporting && !gif.importing
                            onMoved: function (f) {
                                gif.setEndFraction(f);
                            }
                            onTimeEntered: function (t) {
                                const parsed = gif.parseTime(t);
                                if (parsed >= 0)
                                    gif.setEndTime(parsed);
                                endSlider.syncField();
                            }
                            onNudged: function (d) {
                                gif.nudgeEnd(d * gif.duration);
                            }
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 8
                            Repeater {
                                model: [
                                    {
                                        k: qsTr("已选时长"),
                                        v: gif.selectionDurationText
                                    },
                                    {
                                        k: qsTr("起始帧"),
                                        v: gif.startFrameText
                                    },
                                    {
                                        k: qsTr("结束帧"),
                                        v: gif.endFrameText
                                    },
                                    {
                                        k: qsTr("输出帧数"),
                                        v: gif.selectionFramesText
                                    }
                                ]
                                Rectangle {
                                    id: tile
                                    required property var modelData
                                    Layout.fillWidth: true
                                    Layout.preferredHeight: 54
                                    radius: 10
                                    color: window.fieldBg
                                    Column {
                                        anchors.centerIn: parent
                                        spacing: 3
                                        UiLabel {
                                            anchors.horizontalCenter: parent.horizontalCenter
                                            text: tile.modelData.k
                                            color: window.textDim
                                            font.pixelSize: 10
                                        }
                                        UiLabel {
                                            anchors.horizontalCenter: parent.horizontalCenter
                                            text: tile.modelData.v
                                            color: window.textMain
                                            font.pixelSize: 15
                                            font.family: "Consolas"
                                            font.weight: Font.DemiBold
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Card {
                        title: qsTr("导出设置")
                        Layout.fillWidth: true
                        tools: Component {
                            FlatCheck {
                                text: qsTr("循环播放")
                                checked: gif.loopForever
                                enabled: !gif.exporting && !gif.importing
                                onToggled: gif.loopForever = checked
                            }
                        }
                        GridLayout {
                            Layout.fillWidth: true
                            columns: 3
                            columnSpacing: 14
                            rowSpacing: 12
                            enabled: !gif.exporting && !gif.importing
                            OptionGroup {
                                label: qsTr("帧率")
                                FlatCombo {
                                    Layout.fillWidth: true
                                    model: [6, 8, 10, 12, 15, 20, 24, 25, 30]
                                    onActivated: gif.outFps = model[currentIndex]
                                    Component.onCompleted: {
                                        const i = model.indexOf(Math.round(gif.outFps));
                                        currentIndex = i >= 0 ? i : 3;
                                    }
                                }
                            }
                            OptionGroup {
                                label: qsTr("输出宽度")
                                FlatCombo {
                                    Layout.fillWidth: true
                                    model: [qsTr("原始尺寸"), "640 px", "480 px", "320 px"]
                                    onActivated: gif.outWidth = [0, 640, 480, 320][currentIndex]
                                    Component.onCompleted: {
                                        const i = [0, 640, 480, 320].indexOf(gif.outWidth);
                                        currentIndex = i >= 0 ? i : 2;
                                    }
                                }
                            }
                            OptionGroup {
                                label: qsTr("颜色")
                                FlatCombo {
                                    Layout.fillWidth: true
                                    model: ["256", "128", "64", "32"]
                                    onActivated: gif.outColors = [256, 128, 64, 32][currentIndex]
                                    Component.onCompleted: {
                                        const i = [256, 128, 64, 32].indexOf(gif.outColors);
                                        currentIndex = i >= 0 ? i : 0;
                                    }
                                }
                            }
                            OptionGroup {
                                label: qsTr("抖动方式")
                                FlatCombo {
                                    Layout.fillWidth: true
                                    model: [qsTr("无抖动"), "Bayer", "Floyd–Steinberg"]
                                    onActivated: gif.ditherIndex = currentIndex
                                    Component.onCompleted: currentIndex = gif.ditherIndex
                                }
                            }
                            OptionGroup {
                                label: qsTr("播放速度")
                                FlatCombo {
                                    Layout.fillWidth: true
                                    model: ["0.5×", "1×", "1.5×", "2×", "3×"]
                                    onActivated: gif.speed = [0.5, 1, 1.5, 2, 3][currentIndex]
                                    Component.onCompleted: {
                                        const i = [0.5, 1, 1.5, 2, 3].indexOf(gif.speed);
                                        currentIndex = i >= 0 ? i : 1;
                                    }
                                }
                            }
                            OptionGroup {
                                label: qsTr("工作线程")
                                FlatCombo {
                                    Layout.fillWidth: true
                                    readonly property var threadValues: {
                                        const cores = gif.cpuCount;
                                        let values = [0, 1];
                                        for (let v = 2; v <= cores; v *= 2)
                                            values.push(v);
                                        if (values[values.length - 1] !== cores)
                                            values.push(cores);
                                        return values;
                                    }
                                    model: threadValues.map(function (n) {
                                        return n === 0 ? qsTr("自动（%1）").arg(gif.defaultThreadCount) : qsTr("%1 线程").arg(n);
                                    })
                                    onActivated: gif.threadCount = threadValues[currentIndex]
                                    Component.onCompleted: {
                                        const i = threadValues.indexOf(gif.threadCount);
                                        currentIndex = i >= 0 ? i : 0;
                                    }
                                }
                            }
                        }
                    }
                    Card {
                        Layout.fillWidth: true
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 10
                            RowLayout {
                                Layout.fillWidth: true
                                FlatButton {
                                    id: advancedToggle
                                    objectName: "advancedToggle"
                                    checkable: true
                                    flat: true
                                    text: qsTr("高级设置")
                                    symbol: checked ? "−" : "+"
                                    leftPadding: 0
                                    implicitHeight: 28
                                    Accessible.name: checked ? qsTr("收起高级设置") : qsTr("展开高级设置")
                                }
                                Item {
                                    Layout.fillWidth: true
                                }
                                FlatButton {
                                    visible: advancedToggle.checked
                                    text: qsTr("恢复默认")
                                    implicitHeight: 28
                                    flat: true
                                    enabled: !gif.exporting && !gif.importing
                                    onClicked: {
                                        gif.quality = 5;
                                        gif.lossy = 0;
                                        gif.ditherStrength = 100;
                                        gif.dedupe = false;
                                        gif.optimize = false;
                                        gif.tolerance = 0;
                                    }
                                }
                            }
                            ColumnLayout {
                                visible: advancedToggle.checked
                                Layout.fillWidth: true
                                enabled: !gif.exporting && !gif.importing
                                spacing: 8
                                AdvSlider {
                                    label: qsTr("编码质量")
                                    hint: qsTr("提高质量可改善颜色精度，耗时会增加")
                                    from: 1
                                    to: 10
                                    value: gif.quality
                                    onMoved: function (v) {
                                        gif.quality = Math.round(v);
                                    }
                                }
                                AdvSlider {
                                    label: qsTr("有损压缩")
                                    hint: qsTr("更高的压缩会减少色阶")
                                    from: 0
                                    to: 100
                                    step: 5
                                    value: gif.lossy
                                    onMoved: function (v) {
                                        gif.lossy = Math.round(v);
                                    }
                                }
                                AdvSlider {
                                    label: qsTr("抖动强度")
                                    hint: qsTr("0 关闭抖动，100 为默认强度")
                                    from: 0
                                    to: 100
                                    step: 5
                                    value: gif.ditherStrength
                                    onMoved: function (v) {
                                        gif.ditherStrength = Math.round(v);
                                    }
                                }
                                AdvSlider {
                                    visible: gif.optimize
                                    label: qsTr("透明容差")
                                    hint: qsTr("透明优化允许的每通道色差")
                                    from: 0
                                    to: 64
                                    value: gif.tolerance
                                    onMoved: function (v) {
                                        gif.tolerance = Math.round(v);
                                    }
                                }
                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: 24
                                    FlatCheck {
                                        text: qsTr("合并重复帧")
                                        checked: gif.dedupe
                                        onToggled: gif.dedupe = checked
                                    }
                                    FlatCheck {
                                        text: qsTr("透明优化")
                                        checked: gif.optimize
                                        onToggled: gif.optimize = checked
                                    }
                                    Item {
                                        Layout.fillWidth: true
                                    }
                                }
                            }
                        }
                    }
                    Card {
                        title: qsTr("导出 GIF")
                        Layout.fillWidth: true
                        Connections {
                            target: gif
                            function onOutputChanged() {
                                if (!folderField.activeFocus)
                                    folderField.text = gif.outputDir;
                                if (!nameField.activeFocus)
                                    nameField.text = gif.outputName;
                            }
                        }
                        GridLayout {
                            Layout.fillWidth: true
                            columns: 3
                            columnSpacing: 10
                            rowSpacing: 8
                            enabled: !gif.exporting && !gif.importing
                            UiLabel {
                                text: qsTr("保存到")
                                color: window.textDim
                                font.pixelSize: 12
                            }
                            FlatField {
                                id: folderField
                                Layout.fillWidth: true
                                text: gif.outputDir
                                placeholderText: qsTr("选择文件夹")
                                onEditingFinished: gif.outputDir = text
                                Accessible.name: qsTr("保存文件夹")
                            }
                            FlatButton {
                                text: qsTr("浏览")
                                implicitWidth: 72
                                onClicked: folderDialog.open()
                            }
                            UiLabel {
                                text: qsTr("文件名")
                                color: window.textDim
                                font.pixelSize: 12
                            }
                            FlatField {
                                id: nameField
                                Layout.fillWidth: true
                                text: gif.outputName
                                placeholderText: "clip.gif"
                                onEditingFinished: gif.outputName = text
                                Accessible.name: qsTr("GIF 文件名")
                            }
                            Item {
                                implicitWidth: 72
                            }
                        }
                        UiLabel {
                            Layout.fillWidth: true
                            visible: gif.outputProblemText.length > 0
                            text: gif.outputProblemText
                            color: window.warning
                            font.pixelSize: 12
                            wrapMode: Text.WordWrap
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            Layout.topMargin: 4
                            spacing: 10
                            Column {
                                Layout.fillWidth: true
                                spacing: 3
                                UiLabel {
                                    text: qsTr("预计大小")
                                    color: window.textDim
                                    font.pixelSize: 10
                                }
                                UiLabel {
                                    text: !gif.mediaLoaded ? "—" : gif.estimateText.length > 0 ? gif.estimateText : qsTr("估算中…")
                                    color: window.textMain
                                    font.pixelSize: 16
                                    font.weight: Font.DemiBold
                                }
                                ToolTip.visible: estimateHover.hovered && gif.mediaLoaded
                                ToolTip.text: gif.estimateDetailText
                                HoverHandler {
                                    id: estimateHover
                                }
                            }
                            FlatButton {
                                text: qsTr("取消")
                                visible: gif.exporting
                                onClicked: gif.cancelExport()
                            }
                            FlatButton {
                                text: gif.exporting ? qsTr("导出中…") : qsTr("导出 GIF")
                                highlighted: true
                                Layout.preferredWidth: 156
                                Layout.preferredHeight: 40
                                enabled: gif.mediaLoaded && !gif.exporting && !gif.importing && gif.outputProblemText.length === 0
                                onClicked: gif.startExportTo(gif.outputDir, gif.outputName)
                                ToolTip.visible: hovered && gif.mediaLoaded
                                ToolTip.text: gif.outputTargetText
                            }
                        }
                        ProgressBar {
                            id: exportProgress
                            Layout.fillWidth: true
                            visible: gif.exporting
                            from: 0
                            to: 1
                            value: gif.progress
                            implicitHeight: 5
                            background: Rectangle {
                                radius: 2.5
                                color: window.trackBg
                            }
                            contentItem: Item {
                                Rectangle {
                                    width: exportProgress.visualPosition * parent.width
                                    height: parent.height
                                    radius: 2.5
                                    color: window.accent
                                }
                            }
                        }
                        UiLabel {
                            visible: gif.exporting
                            text: gif.phaseText + " · " + Math.round(gif.progress * 100) + "%"
                            color: window.textDim
                            font.pixelSize: 11
                        }
                        UiLabel {
                            Layout.fillWidth: true
                            visible: gif.hasResult
                            text: gif.lastSummary
                            color: window.success
                            font.pixelSize: 12
                            wrapMode: Text.WordWrap
                        }
                        UiLabel {
                            Layout.fillWidth: true
                            visible: gif.lastWarning.length > 0
                            text: gif.lastWarning
                            color: window.warning
                            font.pixelSize: 12
                            wrapMode: Text.WordWrap
                        }
                        UiLabel {
                            Layout.fillWidth: true
                            visible: gif.lastError.length > 0
                            text: gif.lastError
                            color: window.danger
                            font.pixelSize: 12
                            wrapMode: Text.WordWrap
                        }
                    }
                }
            }
        }
    }
    Popup {
        id: importPopup
        parent: Overlay.overlay
        anchors.centerIn: parent
        width: 380
        padding: 24
        modal: true
        closePolicy: Popup.NoAutoClose
        background: Rectangle {
            radius: 16
            color: window.cardBg
            border.color: window.cardLine
        }
        Overlay.modal: Rectangle {
            color: window.dark ? "#99101318" : "#663b4058"
        }
        contentItem: ColumnLayout {
            spacing: 16
            UiLabel {
                text: gif.importing ? qsTr("正在导入视频") : qsTr("导入完成")
                font.pixelSize: 17
                font.weight: Font.DemiBold
                color: window.textMain
            }
            UiLabel {
                Layout.fillWidth: true
                text: gif.importFileName
                elide: Text.ElideMiddle
                color: window.textDim
                font.pixelSize: 12
            }
            ProgressBar {
                id: importBar
                Layout.fillWidth: true
                implicitHeight: 7
                from: 0
                to: 1
                value: gif.importProgress
                Accessible.name: qsTr("视频导入进度")
                background: Rectangle {
                    radius: 3.5
                    color: window.trackBg
                }
                contentItem: Item {
                    Rectangle {
                        width: importBar.visualPosition * parent.width
                        height: parent.height
                        radius: 3.5
                        color: window.accent
                    }
                }
            }
            RowLayout {
                Layout.fillWidth: true
                UiLabel {
                    text: gif.importPhase
                    color: window.textDim
                    font.pixelSize: 12
                    Layout.fillWidth: true
                }
                UiLabel {
                    text: Math.round(gif.importProgress * 100) + "%"
                    color: window.accent
                    font.family: "Consolas"
                    font.pixelSize: 16
                }
            }
        }
        Timer {
            id: importDoneTimer
            interval: 250
            onTriggered: importPopup.close()
        }
        Connections {
            target: gif
            function onImportChanged() {
                if (gif.importing) {
                    importDoneTimer.stop();
                    importPopup.open();
                }
            }
            function onImportFinished(success) {
                if (success)
                    importDoneTimer.restart();
                else
                    importPopup.close();
            }
        }
    }
    Connections {
        target: gif
        function onErrorOccurred(message) {
            errorBanner.text = message;
            errorBanner.opacity = 1;
            errorTimer.restart();
        }
    }
    Rectangle {
        id: errorBanner
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 22
        width: Math.min(parent.width - 64, bannerLabel.implicitWidth + 40)
        height: Math.max(44, bannerLabel.implicitHeight + 24)
        radius: 12
        color: window.dangerBg
        border.color: window.cardLine
        opacity: 0
        visible: opacity > 0.01
        property string text: ""
        Behavior on opacity {
            NumberAnimation {
                duration: 150
            }
        }
        UiLabel {
            id: bannerLabel
            anchors.centerIn: parent
            width: parent.width - 40
            text: errorBanner.text
            color: window.danger
            font.pixelSize: 12
            wrapMode: Text.Wrap
        }
        Timer {
            id: errorTimer
            interval: 6000
            onTriggered: errorBanner.opacity = 0
        }
    }
}
