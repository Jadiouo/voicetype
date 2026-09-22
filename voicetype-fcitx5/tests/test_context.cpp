#include "context.h"

#include <fcitx-utils/utf8.h>

#include <cstdio>
#include <cstdlib>

using namespace voicetype;

static int failures = 0;
#define CHECK(condition) do { if (!(condition)) { \
    std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
    ++failures; } } while (0)

static TextSnapshot atEnd(const std::string &text) {
    const auto length = fcitx::utf8::length(text);
    return {text, length, length};
}

static void testContextBounds() {
    const auto context = boundedContext({"陳柏宇教授研究 GitHub", 3, 0});
    CHECK(context);
    CHECK(context->selection == "陳柏宇");
    CHECK(context->text == "陳柏宇教授研究 GitHub");
    CHECK(!boundedContext({"中", 2, 0}));
    CHECK(!boundedContext({"\xff", 0, 0}));
    std::string longText;
    for (int i = 0; i < 4000; ++i) { longText += "中"; }
    const auto bounded = boundedContext({longText, 3000, 0});
    CHECK(bounded);
    CHECK(fcitx::utf8::validate(bounded->text));
    CHECK(fcitx::utf8::length(bounded->text) == 2048);
    CHECK(bounded->selection.empty()); // oversized selection is not truncated
}

static CorrectionTracker acknowledged(const std::string &dictation,
                                      const std::string &prefix = {},
                                      const std::string &suffix = {}) {
    CorrectionTracker tracker;
    const auto cursor = fcitx::utf8::length(prefix);
    CHECK(tracker.begin({prefix + suffix, cursor, cursor}, dictation, 1000000));
    tracker.observe(atEnd(prefix + dictation + suffix), 1100000);
    return tracker;
}

static void edit(CorrectionTracker &tracker, const std::string &text) {
    tracker.noteUserEditKey(2000000);
    tracker.observe(atEnd(text), 2100000);
}

static void testAutomaticReplacement() {
    const std::string original = "今天我要討論陳博宇教授的研究方向。";
    const std::string corrected = "今天我要討論陳柏宇教授的研究方向。";
    auto tracker = acknowledged(original);
    edit(tracker, corrected);
    CHECK(!tracker.correction(atEnd(corrected), 2200000)); // edit still settling
    const auto correction = tracker.correction(atEnd(corrected), 2600000);
    CHECK(correction);
    CHECK(correction->before == original); // protocol sends full dictation
    CHECK(correction->after == corrected);
    CHECK(!tracker.correction({corrected, 3, 1}, 2600000)); // active selection
    CHECK(!tracker.correction(atEnd(corrected), 700000000)); // expired
    tracker.clear(); // focus change/reset/next dictation
    CHECK(!tracker.correction(atEnd(corrected), 2600000));
}

static void testEnglishLetterCorrection() {
    const std::string original = "我覺得這個東西 mabe 是對的。";
    const std::string corrected = "我覺得這個東西 maybe 是對的。";
    auto tracker = acknowledged(original);
    edit(tracker, corrected);
    CHECK(tracker.correction(atEnd(corrected), 2600000));
    auto backspace = acknowledged(corrected);
    edit(backspace, original);
    CHECK(!backspace.correction(atEnd(original), 2600000));
}

static void testRequiredEvidence() {
    const std::string original = "今天我要討論陳博宇教授的研究方向。";
    const std::string corrected = "今天我要討論陳柏宇教授的研究方向。";
    CorrectionTracker noAcknowledgement;
    CHECK(noAcknowledgement.begin(atEnd(""), original, 1000000));
    edit(noAcknowledgement, corrected);
    CHECK(!noAcknowledgement.correction(atEnd(corrected), 2600000));
    auto noKey = acknowledged(original);
    noKey.observe(atEnd(corrected), 2100000);
    CHECK(!noKey.correction(atEnd(corrected), 2600000));
    // A later real key must not retroactively legitimize a programmatic edit.
    edit(noKey, corrected);
    CHECK(!noKey.correction(atEnd(corrected), 2600000));
    auto unchanged = acknowledged(original);
    edit(unchanged, original);
    CHECK(!unchanged.correction(atEnd(original), 2600000));
}

static void testUnrelatedAndPartialEdits() {
    const std::string original = "今天我要討論陳博宇教授的研究方向。";
    for (const std::string &other : {
             original + "然後新增一段文字。", // normal appended typing
             std::string("插入一段") + original,
             std::string("今天我要討論教授的研究方向。"), // deletion in progress
             std::string("完全不同的一段文字是在修改其他內容。"),
             std::string("今天我要討論很厲害的陳博宇教授的研究方向。")}) {
        auto tracker = acknowledged(original);
        edit(tracker, other);
        CHECK(!tracker.correction(atEnd(other), 2600000));
    }
    auto outside = acknowledged(original, "原本已有文字：", "後面還有文字");
    const auto changedOutside = "改掉已有文字：" + original + "後面還有文字";
    edit(outside, changedOutside);
    CHECK(!outside.correction(atEnd(changedOutside), 2600000));
    auto noAnchors = acknowledged("陳博宇");
    edit(noAnchors, "陳柏宇");
    CHECK(!noAnchors.correction(atEnd("陳柏宇"), 2600000));
    CorrectionTracker selection;
    CHECK(!selection.begin({"已選取的句子", 3, 1}, original, 1000000));
}

int main() {
    testContextBounds();
    testAutomaticReplacement();
    testEnglishLetterCorrection();
    testRequiredEvidence();
    testUnrelatedAndPartialEdits();
    if (failures) { return EXIT_FAILURE; }
    std::puts("all context and correction tests passed");
    return EXIT_SUCCESS;
}
