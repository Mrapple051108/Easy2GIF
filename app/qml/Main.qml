import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs

ApplicationWindow {
    id: window

    width: 1260
    // Tall enough for the whole export form, including the save folder, the
    // file name, the resolved target and the note at the bottom.
    height: 1000
    minimumWidth: 1040
    minimumHeight: 720
    visible: true
    title: qsTr("Easy2GIF — 视频转 GIF")
    color: "#0b1219"

    // ---------------------------------------------------------------------
    // Theme
    // ---------------------------------------------------------------------
    readonly property color accent: "#4fc3f7"
    readonly property color textMain: "#e6eef6"
    readonly property color textDim: "#7d8fa3"
    readonly property color cardBg: "#111a23"
    readonly property color cardLine: "#1e2b38"
    readonly property color fieldBg: "#141d27"

    function pathToUrl(p) {
        return "file:///" + p.replace(/\\/g, "/")
    }

    // ---------------------------------------------------------------------
    // Reusable pieces
    // ---------------------------------------------------------------------

    // A titled panel. Usages set an explicit height, so the inner column can
    // simply fill it without any implicit-size feedback loop.
    component Card: Rectangle {
        id: card
        property string title: ""
        default property alias content: inner.data
        radius: 10
        color: window.cardBg
        border.color: window.cardLine
        border.width: 1

        ColumnLayout {
            id: inner
            anchors.fill: parent
            anchors.margins: 12
            spacing: 9

            Label {
                visible: card.title.length > 0
                text: card.title
                color: window.textMain
                font.pixelSize: 14
                font.bold: true
                Layout.fillWidth: true
            }
        }
    }

    // A dark combo box that does not depend on a platform style plugin.
    component DarkCombo: ComboBox {
        id: ctl
        implicitWidth: 132
        implicitHeight: 30
        font.pixelSize: 12

        // Long entries such as "Floyd–Steinberg（最佳）" must never be cut off,
        // so the popup is measured against the widest one instead of being
        // pinned to the width of the closed field.
        readonly property real popupWidth: {
            let widest = ctl.width
            const n = ctl.count
            for (let i = 0; i < n; ++i) {
                measure.text = ctl.textAt(i)
                widest = Math.max(widest, measure.advanceWidth + 30)
            }
            measure.text = ""
            return Math.min(widest, Math.max(240, window.width - 80))
        }

        TextMetrics {
            id: measure
            font.pixelSize: 12
        }

        contentItem: Text {
            leftPadding: 10
            rightPadding: 22
            text: ctl.displayText
            color: window.textMain
            font.pixelSize: 12
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }

        indicator: Text {
            x: ctl.width - width - 9
            y: (ctl.height - height) / 2
            text: "▾"
            color: window.textDim
            font.pixelSize: 12
        }

        background: Rectangle {
            radius: 5
            color: ctl.enabled ? window.fieldBg : "#0e151c"
            border.color: ctl.activeFocus ? window.accent : "#2b3a49"
            border.width: 1
        }

        delegate: ItemDelegate {
            id: item
            required property var modelData
            required property int index
            width: Math.max(ctl.width, ctl.popupWidth) - 8
            highlighted: ctl.highlightedIndex === item.index

            contentItem: Text {
                text: item.modelData
                color: window.textMain
                font.pixelSize: 12
                verticalAlignment: Text.AlignVCenter
                wrapMode: Text.NoWrap
            }
            background: Rectangle {
                radius: 4
                color: item.highlighted ? "#22384a" : "transparent"
            }
        }

        popup: Popup {
            y: ctl.height + 2
            width: Math.max(ctl.width, ctl.popupWidth)
            implicitHeight: Math.min(contentItem.implicitHeight + 8, 280)
            padding: 4

            contentItem: ListView {
                clip: true
                implicitHeight: contentHeight
                model: ctl.popup.visible ? ctl.delegateModel : null
                currentIndex: ctl.highlightedIndex
                boundsBehavior: Flickable.StopAtBounds
                ScrollIndicator.vertical: ScrollIndicator {}
            }
            background: Rectangle {
                radius: 6
                color: "#16222e"
                border.color: "#2b3a49"
                border.width: 1
            }
        }
    }

    // One labelled slider row of the advanced section. The value is driven by a
    // Binding rather than a plain assignment so that dragging the handle does
    // not sever the link to the bridge property (which would leave the row
    // stuck wherever it was last touched).
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
        spacing: 8

        Label {
            text: adv.label
            color: window.textDim
            font.pixelSize: 12
            Layout.preferredWidth: 76
        }

        Slider {
            id: slider
            Layout.fillWidth: true
            from: adv.from
            to: adv.to
            stepSize: adv.step
            Binding {
                target: slider
                property: "value"
                value: adv.value
                restoreMode: Binding.RestoreNone
            }
            onMoved: adv.moved(value)
        }

        Label {
            text: Math.round(adv.value)
            color: window.textMain
            font.pixelSize: 12
            horizontalAlignment: Text.AlignRight
            Layout.preferredWidth: 34
        }

        Label {
            text: adv.hint
            color: window.textDim
            font.pixelSize: 11
            elide: Text.ElideRight
            Layout.preferredWidth: 210
        }
    }

    // ---------------------------------------------------------------------
    // Dialogs
    // ---------------------------------------------------------------------
    FileDialog {
        id: openDialog
        title: qsTr("选择视频文件")
        nameFilters: [
            "视频文件 (*.mp4 *.mkv *.mov *.avi *.webm *.m4v *.wmv *.flv *.ts *.mpg *.mpeg *.3gp)",
            "所有文件 (*)"
        ]
        onAccepted: gif.openVideo(selectedFile.toString())
    }

    // Picking the folder and typing the name replaced a SaveFile dialog: the
    // user could not tell what the dialog had resolved to, and a save that
    // failed only said "拒绝访问". Now the target path is on screen before the
    // export starts, and the kernel is asked whether it is writable.
    FolderDialog {
        id: folderDialog
        title: qsTr("选择保存文件夹")
        currentFolder: gif.outputDir.length > 0 ? window.pathToUrl(gif.outputDir) : ""
        onAccepted: gif.outputDir = gif.toLocalPath(selectedFolder.toString())
    }

    // ---------------------------------------------------------------------
    // Layout
    // ---------------------------------------------------------------------
    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 16
        spacing: 12

        // ---- header -----------------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Label {
                text: "Easy2GIF"
                color: window.accent
                font.pixelSize: 22
                font.bold: true
            }
            Label {
                text: qsTr("视频转 GIF · Rust 内核 / Qt 交互层")
                color: window.textDim
                font.pixelSize: 12
                Layout.alignment: Qt.AlignVCenter
            }

            Item { Layout.fillWidth: true }

            Rectangle {
                Layout.preferredHeight: 26
                Layout.preferredWidth: toolLabel.implicitWidth + 20
                radius: 13
                color: gif.toolsReady ? "#123024" : "#33221a"
                border.color: gif.toolsReady ? "#2e7d5b" : "#a2603a"
                border.width: 1

                Label {
                    id: toolLabel
                    anchors.centerIn: parent
                    text: gif.toolsStatus
                    color: gif.toolsReady ? "#7fd8a8" : "#e0a878"
                    font.pixelSize: 11
                }
            }

            // Windows refuses every write to an ordinary folder from a
            // Low-integrity process, whatever that folder's ACL says — so a
            // sandboxed launch makes export fail for a reason nothing in the
            // file system explains. Say it before the user tries, not after.
            Rectangle {
                visible: gif.sandboxed
                Layout.preferredHeight: 26
                Layout.preferredWidth: sandboxLabel.implicitWidth + 20
                radius: 13
                color: "#33291a"
                border.color: "#a2823a"
                border.width: 1

                Label {
                    id: sandboxLabel
                    anchors.centerIn: parent
                    text: qsTr("沙箱运行 · 导出受限")
                    color: "#e0c070"
                    font.pixelSize: 11
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    ToolTip.visible: containsMouse
                    ToolTip.delay: 300
                    // QML's JS parser has no implicit string-literal
                    // concatenation, so the parts are joined with '+'.
                    ToolTip.text: qsTr("本程序正以「低完整性」身份运行，系统不允许它写入普通文件夹。 ")
                                  + qsTr("请从资源管理器或开始菜单直接启动本程序，导出才能正常工作。")
                }
            }

            Label {
                text: "内核 " + gif.coreVersion
                color: window.textDim
                font.pixelSize: 11
            }
        }

        // ---- body -------------------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 14

            // =============== left column ================================
            // The min/max pair matters: a Card is a Rectangle, so it reports no
            // implicit size of its own and the column would otherwise be free to
            // stretch and swallow the whole row.
            ColumnLayout {
                Layout.preferredWidth: 348
                Layout.minimumWidth: 300
                Layout.maximumWidth: 400
                Layout.fillHeight: true
                spacing: 12

                // preview
                Card {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 286
                    title: qsTr("起始帧预览")

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        color: "#080d12"
                        radius: 7
                        border.color: "#1c2833"
                        border.width: 1

                        Image {
                            anchors.fill: parent
                            anchors.margins: 1
                            fillMode: Image.PreserveAspectFit
                            source: gif.mediaLoaded
                                    ? "image://gifcore/poster?r=" + gif.posterRevision
                                    : ""
                            cache: false
                        }

                        Label {
                            anchors.centerIn: parent
                            visible: !gif.mediaLoaded
                            text: qsTr("尚未载入视频")
                            color: "#4d5c6b"
                            font.pixelSize: 13
                        }
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.mediaLoaded
                        text: gif.startTimeText + "  ·  第 " + gif.startFrameText + " 帧"
                        color: window.textDim
                        font.pixelSize: 11
                        horizontalAlignment: Text.AlignHCenter
                    }
                }

                // media info
                Card {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 250
                    title: qsTr("视频信息")

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: 10
                        rowSpacing: 7

                        Label { text: qsTr("文件"); color: window.textDim; font.pixelSize: 12 }
                        Label {
                            Layout.fillWidth: true
                            text: gif.mediaLoaded ? gif.fileName : "—"
                            color: window.textMain
                            font.pixelSize: 12
                            elide: Text.ElideMiddle
                        }

                        Label { text: qsTr("分辨率"); color: window.textDim; font.pixelSize: 12 }
                        Label { text: gif.resolutionText; color: window.textMain; font.pixelSize: 12 }

                        Label { text: qsTr("帧率"); color: window.textDim; font.pixelSize: 12 }
                        Label { text: gif.fpsText + " fps"; color: window.textMain; font.pixelSize: 12 }

                        Label { text: qsTr("总时长"); color: window.textDim; font.pixelSize: 12 }
                        Label { text: gif.durationText; color: window.textMain; font.pixelSize: 12 }

                        Label { text: qsTr("总帧数"); color: window.textDim; font.pixelSize: 12 }
                        Label { text: gif.frameCountText; color: window.textMain; font.pixelSize: 12 }

                        Label { text: qsTr("编码"); color: window.textDim; font.pixelSize: 12 }
                        Label {
                            text: gif.mediaLoaded && gif.videoCodec.length > 0 ? gif.videoCodec : "—"
                            color: window.textMain
                            font.pixelSize: 12
                        }

                        Label { text: qsTr("音轨"); color: window.textDim; font.pixelSize: 12 }
                        Label {
                            text: gif.mediaLoaded ? (gif.hasAudio ? qsTr("有（GIF 不含音频）") : qsTr("无"))
                                                  : "—"
                            color: window.textMain
                            font.pixelSize: 12
                        }
                    }

                    Item { Layout.fillHeight: true }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8

                        Button {
                            Layout.fillWidth: true
                            text: qsTr("打开视频…")
                            highlighted: true
                            onClicked: openDialog.open()
                        }
                        Button {
                            Layout.preferredWidth: 76
                            text: qsTr("关闭")
                            enabled: gif.mediaLoaded && !gif.exporting
                            onClicked: gif.closeVideo()
                        }
                    }
                }
            }

            // =============== right column ===============================
            // The column can outgrow the window — the advanced section alone
            // adds several rows when it is opened — so it scrolls instead of
            // squeezing the export card off the bottom edge.
            ScrollView {
                id: rightScroll
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.minimumWidth: 620
                clip: true
                contentWidth: availableWidth
                ScrollBar.vertical.policy: ScrollBar.AsNeeded

                ColumnLayout {
                    width: rightScroll.availableWidth
                    spacing: 12

                // ---- selection ------------------------------------------
                Card {
                    Layout.fillWidth: true
                    // Trimmed a little: the section below it grew, and the
                    // column scrolls, so every pixel saved here is a pixel of
                    // the export card that stays on screen.
                    Layout.preferredHeight: 400
                    title: qsTr("选择区间 — 拖动竖线，或直接输入时间点")

                    // timeline overview with both handles
                    Item {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 92

                        Image {
                            id: stripImage
                            anchors.fill: parent
                            fillMode: Image.Stretch
                            cache: false
                            source: gif.filmstripReady
                                    ? "image://gifcore/strip?r=" + gif.filmstripRevision
                                    : ""
                        }

                        Rectangle {
                            anchors.fill: parent
                            color: "#0d151d"
                            visible: !gif.filmstripReady
                        }

                        // dim everything, then lift the dimming back off the
                        // selection so the chosen range reads as "live"
                        Rectangle {
                            x: 0
                            width: Math.max(0, Math.round(gif.startFraction * parent.width))
                            height: parent.height
                            color: "#05090d"
                            opacity: 0.68
                        }
                        Rectangle {
                            x: Math.round(gif.endFraction * parent.width)
                            width: Math.max(0, parent.width - x)
                            height: parent.height
                            color: "#05090d"
                            opacity: 0.68
                        }

                        // outline of the live part of the clip
                        Rectangle {
                            x: Math.round(gif.startFraction * parent.width)
                            width: Math.max(0, Math.round((gif.endFraction - gif.startFraction)
                                                           * parent.width))
                            height: parent.height
                            color: "transparent"
                            border.color: window.accent
                            border.width: 2
                        }

                        // start handle
                        Rectangle {
                            x: Math.round(gif.startFraction * parent.width) - 1
                            width: 3
                            height: parent.height
                            color: "#4fc3f7"
                            Rectangle {
                                width: 16; height: 16; radius: 3
                                color: "#4fc3f7"
                                anchors.horizontalCenter: parent.horizontalCenter
                                anchors.top: parent.top
                                anchors.topMargin: 2
                            }
                        }

                        // end handle
                        Rectangle {
                            x: Math.round(gif.endFraction * parent.width) - 1
                            width: 3
                            height: parent.height
                            color: "#ffb74d"
                            Rectangle {
                                width: 16; height: 16; radius: 3
                                color: "#ffb74d"
                                anchors.horizontalCenter: parent.horizontalCenter
                                anchors.bottom: parent.bottom
                                anchors.bottomMargin: 2
                            }
                        }

                        // one mouse area drives whichever handle is nearer
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.SizeHorCursor
                            property bool grabbingStart: true

                            function frac(px) {
                                return Math.max(0, Math.min(1, px / Math.max(1, width)))
                            }

                            onPressed: {
                                var f = frac(mouse.x)
                                grabbingStart = Math.abs(f - gif.startFraction)
                                                <= Math.abs(f - gif.endFraction)
                                if (grabbingStart) gif.setStartFraction(f)
                                else gif.setEndFraction(f)
                            }
                            onPositionChanged: if (pressed) {
                                var f = frac(mouse.x)
                                if (grabbingStart) gif.setStartFraction(f)
                                else gif.setEndFraction(f)
                            }
                        }
                    }

                    // ---- the two bars -----------------------------------
                    FrameSlider {
                        id: startSlider
                        Layout.fillWidth: true
                        label: qsTr("开始帧")
                        accent: "#4fc3f7"
                        hint: qsTr("支持 00:01.500 / 1:02 / 150f")
                        fraction: gif.startFraction
                        timeText: gif.startTimeText
                        frameText: gif.startFrameText
                        enabled: gif.mediaLoaded
                        onMoved: function (f) { gif.setStartFraction(f) }
                        onTimeEntered: function (t) {
                            var parsed = gif.parseTime(t)
                            if (parsed >= 0) gif.setStartTime(parsed)
                            startSlider.syncField()
                        }
                        onNudged: function (d) { gif.nudgeStart(d * gif.duration) }
                    }

                    FrameSlider {
                        id: endSlider
                        Layout.fillWidth: true
                        label: qsTr("结束帧")
                        accent: "#ffb74d"
                        hint: qsTr("留空或输入更大值以扩展")
                        fraction: gif.endFraction
                        timeText: gif.endTimeText
                        frameText: gif.endFrameText
                        enabled: gif.mediaLoaded
                        onMoved: function (f) { gif.setEndFraction(f) }
                        onTimeEntered: function (t) {
                            var parsed = gif.parseTime(t)
                            if (parsed >= 0) gif.setEndTime(parsed)
                            endSlider.syncField()
                        }
                        onNudged: function (d) { gif.nudgeEnd(d * gif.duration) }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8
                        Button {
                            text: qsTr("全片")
                            enabled: gif.mediaLoaded
                            onClicked: gif.selectWholeClip()
                        }
                        Button {
                            text: qsTr("单帧")
                            enabled: gif.mediaLoaded
                            onClicked: gif.clearSelection()
                        }
                        Item { Layout.fillWidth: true }
                        Label {
                            text: qsTr("选区内共 %1 帧（按输出帧率计算）").arg(gif.selectionFramesText)
                            color: window.textDim
                            font.pixelSize: 11
                        }
                    }

                    // ---- the headline readout ---------------------------
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 10

                        Repeater {
                            model: [
                                { k: qsTr("选区总时长"), v: gif.selectionDurationText, c: "#4fc3f7" },
                                { k: qsTr("起始帧"),     v: gif.startFrameText,      c: "#4fc3f7" },
                                { k: qsTr("结束帧"),     v: gif.endFrameText,        c: "#ffb74d" },
                                { k: qsTr("输出帧数"),   v: gif.selectionFramesText, c: "#9ccc65" }
                            ]

                            Rectangle {
                                id: tile
                                required property var modelData
                                Layout.fillWidth: true
                                Layout.preferredHeight: 56
                                radius: 8
                                color: "#0e1a24"
                                border.color: "#1f2f3d"
                                border.width: 1

                                ColumnLayout {
                                    anchors.centerIn: parent
                                    spacing: 1
                                    Label {
                                        Layout.alignment: Qt.AlignHCenter
                                        text: tile.modelData.k
                                        color: window.textDim
                                        font.pixelSize: 11
                                    }
                                    Label {
                                        Layout.alignment: Qt.AlignHCenter
                                        text: tile.modelData.v
                                        color: tile.modelData.c
                                        font.pixelSize: 17
                                        font.bold: true
                                        font.family: "Consolas"
                                    }
                                }
                            }
                        }
                    }
                }

                // ---- export settings ------------------------------------
                Card {
                    Layout.fillWidth: true
                    // Three rows of 30 px controls, two 6 px gaps, plus the card
                    // margins, the title and its 9 px spacing.
                    Layout.preferredHeight: 152
                    title: qsTr("导出设置")

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 6
                        columnSpacing: 10
                        rowSpacing: 6

                        Label { text: qsTr("帧率"); color: window.textDim; font.pixelSize: 12 }
                        DarkCombo {
                            id: fpsCombo
                            model: [6, 8, 10, 12, 15, 20, 24, 25, 30]
                            onActivated: gif.outFps = model[currentIndex]
                            Component.onCompleted: {
                                var i = model.indexOf(Math.round(gif.outFps))
                                currentIndex = i >= 0 ? i : 3
                            }
                        }

                        Label { text: qsTr("宽度"); color: window.textDim; font.pixelSize: 12 }
                        DarkCombo {
                            id: widthCombo
                            model: [qsTr("原始尺寸"), "640 px", "480 px", "320 px"]
                            onActivated: gif.outWidth = [0, 640, 480, 320][currentIndex]
                            Component.onCompleted: {
                                var i = [0, 640, 480, 320].indexOf(gif.outWidth)
                                currentIndex = i >= 0 ? i : 2
                            }
                        }

                        Label { text: qsTr("颜色数"); color: window.textDim; font.pixelSize: 12 }
                        DarkCombo {
                            id: colorCombo
                            model: ["256", "128", "64", "32"]
                            onActivated: gif.outColors = [256, 128, 64, 32][currentIndex]
                            Component.onCompleted: {
                                var i = [256, 128, 64, 32].indexOf(gif.outColors)
                                currentIndex = i >= 0 ? i : 0
                            }
                        }

                        Label { text: qsTr("抖动"); color: window.textDim; font.pixelSize: 12 }
                        DarkCombo {
                            id: ditherCombo
                            model: [qsTr("无（最快）"), qsTr("有序抖动"), qsTr("Floyd–Steinberg（最佳）")]
                            onActivated: gif.ditherIndex = currentIndex
                            Component.onCompleted: currentIndex = gif.ditherIndex
                        }

                        Label { text: qsTr("速度"); color: window.textDim; font.pixelSize: 12 }
                        DarkCombo {
                            id: speedCombo
                            model: ["0.5×", "1×", "1.5×", "2×", "3×"]
                            onActivated: gif.speed = [0.5, 1, 1.5, 2, 3][currentIndex]
                            Component.onCompleted: {
                                var i = [0.5, 1, 1.5, 2, 3].indexOf(gif.speed)
                                currentIndex = i >= 0 ? i : 1
                            }
                        }

                        Label { text: qsTr("线程"); color: window.textDim; font.pixelSize: 12 }
                        DarkCombo {
                            id: threadCombo
                            // 0 asks the kernel to decide (half the cores); the
                            // rest are explicit counts, doubling up to the
                            // machine's core count so the list stays short.
                            readonly property var threadValues: {
                                var cores = gif.cpuCount
                                var values = [0, 1]
                                var v = 2
                                while (v <= cores) {
                                    values.push(v)
                                    v *= 2
                                }
                                if (values[values.length - 1] !== cores)
                                    values.push(cores)
                                return values
                            }
                            model: {
                                var names = []
                                for (var i = 0; i < threadValues.length; i++) {
                                    var n = threadValues[i]
                                    names.push(n === 0
                                        ? qsTr("自动（%1）").arg(gif.defaultThreadCount)
                                        : qsTr("%1 线程").arg(n))
                                }
                                return names
                            }
                            onActivated: gif.threadCount = threadValues[currentIndex]
                            Component.onCompleted: {
                                var i = threadValues.indexOf(gif.threadCount)
                                currentIndex = i >= 0 ? i : 0
                            }
                        }

                        CheckBox {
                            id: loopCheck
                            text: qsTr("循环播放")
                            checked: gif.loopForever
                            onToggled: gif.loopForever = checked
                            Layout.columnSpan: 6
                        }
                    }
                }

                // ---- advanced settings --------------------------------
                // Only revealed once 「高级设置」 is ticked. Every default here
                // reproduces the historic output byte for byte, so the section
                // is opt-in rather than something to be afraid of.
                Card {
                    id: advCard
                    Layout.fillWidth: true
                    // Grows with the section: only the checkbox row is laid out
                    // while it is closed, and every slider is dropped from the
                    // layout (`visible: false`) until it is ticked.
                    // No title: the checkbox in the row below already names the
                    // section, and the export card below it can use those pixels.
                    Layout.preferredHeight: 24 + advBody.implicitHeight

                    ColumnLayout {
                        id: advBody
                        Layout.fillWidth: true
                        spacing: 7

                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 10

                            CheckBox {
                                id: advCheck
                                text: qsTr("高级设置")
                                checked: false
                            }

                            Label {
                                Layout.fillWidth: true
                                text: gif.qualitySummary
                                color: window.textDim
                                font.pixelSize: 11
                                elide: Text.ElideRight
                            }

                            Button {
                                visible: advCheck.checked
                                text: qsTr("恢复默认")
                                Layout.preferredWidth: 96
                                Layout.preferredHeight: 30
                                onClicked: {
                                    gif.quality = 5
                                    gif.lossy = 0
                                    gif.ditherStrength = 100
                                    gif.dedupe = false
                                    gif.optimize = false
                                    gif.tolerance = 0
                                }
                            }
                        }

                        AdvSlider {
                            visible: advCheck.checked
                            label: qsTr("编码质量")
                            hint: qsTr("调色板聚类迭代，越高越准、越慢")
                            from: 1
                            to: 10
                            step: 1
                            value: gif.quality
                            onMoved: gif.quality = Math.round(v)
                        }

                        AdvSlider {
                            visible: advCheck.checked
                            label: qsTr("有损压缩")
                            hint: qsTr("先量化色阶再取色，明显更小")
                            from: 0
                            to: 100
                            step: 5
                            value: gif.lossy
                            onMoved: gif.lossy = Math.round(v)
                        }

                        AdvSlider {
                            visible: advCheck.checked
                            label: qsTr("抖动强度")
                            hint: qsTr("误差扩散幅度，100 为当前默认")
                            from: 0
                            to: 100
                            step: 5
                            value: gif.ditherStrength
                            onMoved: gif.ditherStrength = Math.round(v)
                        }

                        AdvSlider {
                            visible: advCheck.checked && gif.optimize
                            label: qsTr("透明容差")
                            hint: qsTr("帧间透明优化允许的每通道色差")
                            from: 0
                            to: 64
                            step: 1
                            value: gif.tolerance
                            onMoved: gif.tolerance = Math.round(v)
                        }

                        RowLayout {
                            visible: advCheck.checked
                            Layout.fillWidth: true
                            spacing: 18

                            CheckBox {
                                text: qsTr("复用重复帧")
                                checked: gif.dedupe
                                onToggled: gif.dedupe = checked
                            }

                            CheckBox {
                                text: qsTr("帧间透明优化")
                                checked: gif.optimize
                                onToggled: gif.optimize = checked
                            }

                            Item { Layout.fillWidth: true }
                        }
                    }
                }

                // ---- export action --------------------------------------
                // Sits last in the column on purpose: every card above it is part of
                // what this one is about to write.
                Card {
                    Layout.fillWidth: true
                    // A minimum rather than a fixed height: the column lives in a
                    // scroll view, so fillHeight has nothing to fill, but the
                    // card must still be tall enough for the progress block and
                    // the result lines to appear without clipping.
                    Layout.fillHeight: true
                    // Tall enough for the progress block and the result lines, but only
                    // reserved while there is something to show: the column is a
                    // scroll view and the button must stay reachable when idle.
                    Layout.minimumHeight: gif.exporting || gif.hasResult ? 348 : 264
                    title: qsTr("导出")

                    // ---- where the GIF goes -----------------------------
                    Connections {
                        target: gif
                        // Only push the value into a field the user is not
                        // typing in, or the kernel would fight the typist.
                        function onOutputChanged() {
                            if (!folderField.activeFocus)
                                folderField.text = gif.outputDir
                            if (!nameField.activeFocus)
                                nameField.text = gif.outputName
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 3
                        columnSpacing: 8
                        rowSpacing: 8

                        Label {
                            text: qsTr("保存到")
                            color: window.textDim
                            font.pixelSize: 12
                        }

                        TextField {
                            id: folderField
                            Layout.fillWidth: true
                            Layout.preferredHeight: 30
                            text: gif.outputDir
                            placeholderText: qsTr("选择 GIF 保存到哪个文件夹")
                            selectByMouse: true
                            color: window.textMain
                            font.pixelSize: 12
                            onEditingFinished: gif.outputDir = text
                            background: Rectangle {
                                radius: 4
                                color: window.fieldBg
                                border.width: 1
                                border.color: folderField.activeFocus
                                              ? window.accent : window.cardLine
                            }
                        }

                        Button {
                            Layout.preferredWidth: 88
                            Layout.preferredHeight: 30
                            text: qsTr("浏览…")
                            enabled: !gif.exporting
                            onClicked: folderDialog.open()
                        }

                        Label {
                            text: qsTr("文件名")
                            color: window.textDim
                            font.pixelSize: 12
                        }

                        TextField {
                            id: nameField
                            Layout.fillWidth: true
                            Layout.preferredHeight: 30
                            text: gif.outputName
                            placeholderText: qsTr("例如 clip.gif")
                            selectByMouse: true
                            color: window.textMain
                            font.pixelSize: 12
                            onEditingFinished: gif.outputName = text
                            background: Rectangle {
                                radius: 4
                                color: window.fieldBg
                                border.width: 1
                                border.color: nameField.activeFocus
                                              ? window.accent : window.cardLine
                            }
                        }

                        Item { Layout.preferredWidth: 88 }
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.outputTargetText.length > 0
                        text: qsTr("将写入 %1").arg(gif.outputTargetText)
                        color: window.textDim
                        font.pixelSize: 11
                        elide: Text.ElideMiddle
                    }

                    // ---- projected size --------------------------------
                    // Measured on a worker thread by the kernel and refreshed
                    // whenever anything that changes the result moves, so the
                    // GUI thread is never blocked waiting for it.
                    RowLayout {
                        Layout.fillWidth: true
                        visible: gif.mediaLoaded
                        spacing: 8

                        Label {
                            text: qsTr("预计大小")
                            color: window.textDim
                            font.pixelSize: 12
                        }

                        Label {
                            text: gif.estimateText.length > 0
                                  ? gif.estimateText
                                  : qsTr("估算中…")
                            color: window.accent
                            font.pixelSize: 14
                            font.bold: true
                        }

                        Label {
                            Layout.fillWidth: true
                            text: gif.estimating && gif.estimateDetailText.length > 0
                                  ? qsTr("（%1 · 更新中…）").arg(gif.estimateDetailText)
                                  : gif.estimateDetailText
                            color: window.textDim
                            font.pixelSize: 11
                            elide: Text.ElideRight
                        }
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.outputProblemText.length > 0
                        text: "⚠ " + gif.outputProblemText
                        color: "#e0c070"
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 10

                        Label {
                            Layout.fillWidth: true
                            // Chained one-argument arg() calls on purpose: the
                            // multi-argument QString::arg overload is not
                            // callable from QML, and a failed call silently
                            // freezes the label at its first evaluation.
                            //
                            // outFps, not fpsText: the latter is the source
                            // video's rate, and the frame count above is
                            // computed at the output rate.
                            text: gif.mediaLoaded
                                  ? qsTr("选区 %1，共 %2 帧 → %3 fps · 编码 %4")
                                        .arg(gif.selectionDurationText)
                                        .arg(gif.selectionFramesText)
                                        .arg(Number(gif.outFps).toFixed(0))
                                        .arg(gif.threadSummary)
                                  : qsTr("请先打开一个视频文件")
                            color: window.textDim
                            font.pixelSize: 12
                            elide: Text.ElideRight
                        }

                        Button {
                            Layout.preferredWidth: 168
                            Layout.preferredHeight: 38
                            text: gif.exporting ? qsTr("导出中…") : qsTr("导出")
                            highlighted: true
                            // Refuse to start a long export into a place the
                            // kernel already knows it cannot write.
                            enabled: gif.mediaLoaded && !gif.exporting
                                     && gif.outputProblemText.length === 0
                            onClicked: gif.startExportTo(gif.outputDir, gif.outputName)
                        }

                        Button {
                            Layout.preferredWidth: 88
                            Layout.preferredHeight: 38
                            text: qsTr("取消")
                            visible: gif.exporting
                            onClicked: gif.cancelExport()
                        }
                    }

                    ProgressBar {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 20
                        from: 0
                        to: 1
                        value: gif.progress
                        visible: gif.exporting || gif.progress > 0
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.exporting
                        text: gif.phaseText + "  ·  "
                              + Math.round(gif.progress * 100) + "%"
                        color: window.accent
                        font.pixelSize: 12
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.hasResult
                        text: "✅ " + gif.lastSummary
                        color: "#7fd8a8"
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.lastWarning.length > 0
                        text: "⚠ " + gif.lastWarning
                        color: "#e0c070"
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }

                    Label {
                        Layout.fillWidth: true
                        visible: gif.lastError.length > 0
                        text: "⛔ " + gif.lastError
                        color: "#ef8a8a"
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }

                    Item { Layout.fillHeight: true }

                    Label {
                        Layout.fillWidth: true
                        text: qsTr("所有依赖（Qt 运行库与视频引擎）均已随程序打包，无需另装任何插件。")
                        color: "#4d5c6b"
                        font.pixelSize: 11
                        wrapMode: Text.WordWrap
                    }
                } // 导出 card

                } // right column
            } // right column ScrollView
        } // selection row
    } // page column

    // Surface kernel-level failures as a transient banner.
    Connections {
        target: gif
        function onErrorOccurred(message) {
            errorBanner.text = message
            errorBanner.opacity = 1
            errorTimer.restart()
        }
    }

    Rectangle {
        id: errorBanner
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 22
        width: Math.min(parent.width - 80, bannerLabel.implicitWidth + 36)
        height: 44
        radius: 8
        color: "#3a1d1d"
        border.color: "#8f4040"
        border.width: 1
        opacity: 0
        visible: opacity > 0.01

        property string text: ""

        Behavior on opacity { NumberAnimation { duration: 180 } }

        Label {
            id: bannerLabel
            anchors.centerIn: parent
            text: errorBanner.text
            color: "#f0b0b0"
            font.pixelSize: 12
        }

        Timer {
            id: errorTimer
            interval: 6000
            onTriggered: errorBanner.opacity = 0
        }
    }
}
