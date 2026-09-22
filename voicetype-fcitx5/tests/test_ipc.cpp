// NDJSON 編解碼的單元測試 (SDD §8.2)。
//
// 這是 addon 裡唯一有實質分支邏輯的部分, 也是唯一會直接處理使用者
// 轉錄內容的地方——解錯就是亂碼進到編輯器, 所以值得測。
// 刻意不引入 gtest: 少一個建置依賴, 斷言需求也很單純。

#include "ipc.h"

#include <cstdio>
#include <cstdlib>
#include <string>

static int g_failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__,       \
                         #cond);                                               \
            ++g_failures;                                                      \
        }                                                                      \
    } while (0)

#define CHECK_EQ(a, b)                                                         \
    do {                                                                       \
        auto _a = (a);                                                         \
        auto _b = (b);                                                         \
        if (!(_a == _b)) {                                                     \
            std::fprintf(stderr, "FAIL %s:%d: %s == %s\n", __FILE__, __LINE__, \
                         #a, #b);                                              \
            ++g_failures;                                                      \
        }                                                                      \
    } while (0)

#define CHECK_STR_EQ(a, b)                                                     \
    do {                                                                       \
        std::string _a = (a);                                                  \
        std::string _b = (b);                                                  \
        if (_a != _b) {                                                        \
            std::fprintf(stderr, "FAIL %s:%d: got [%s], want [%s]\n",          \
                         __FILE__, __LINE__, _a.c_str(), _b.c_str());          \
            ++g_failures;                                                      \
        }                                                                      \
    } while (0)

using namespace voicetype;

static void testSerializeStart() {
    IpcMessage m;
    m.type = "start";
    m.setSession(42);
    m.program = "kitty";
    m.isPassword = false;
    m.hasIsPassword = true;
    CHECK_STR_EQ(
        serialize(m),
        R"({"type":"start","session":42,"program":"kitty","is_password":false})");
}

static void testSerializeStop() {
    IpcMessage m;
    m.type = "stop";
    m.setSession(7);
    CHECK_STR_EQ(serialize(m), R"({"type":"stop","session":7})");
}

static void testContextAndCorrectionWire() {
    IpcMessage start;
    start.type = "start";
    start.setSession(44);
    start.contextId = "instance-input-context";
    start.contextText = "前文：陳柏宇教授\n研究 GitHub";
    start.selectedText = "陳柏宇";
    IpcMessage back;
    CHECK(parse(serialize(start), back));
    CHECK_STR_EQ(back.contextId, start.contextId);
    CHECK_STR_EQ(back.contextText, start.contextText);
    CHECK_STR_EQ(back.selectedText, start.selectedText);

    IpcMessage correction;
    correction.type = "correction";
    correction.setSession(44);
    correction.program = "editor";
    correction.contextId = "instance-input-context";
    correction.before = "請把東西 push 到 gthub。";
    correction.after = "請把東西 push 到 GitHub。";
    correction.hasConfirmed = true;
    correction.confirmed = false;
    CHECK_STR_EQ(serialize(correction),
        R"({"type":"correction","session":44,"program":"editor","context_id":"instance-input-context","before":"請把東西 push 到 gthub。","after":"請把東西 push 到 GitHub。","confirmed":false})");
    CHECK(parse(serialize(correction), back));
    CHECK_STR_EQ(back.before, correction.before);
    CHECK_STR_EQ(back.after, correction.after);
    CHECK(back.hasConfirmed && !back.confirmed);
    correction.confirmed = true;
    CHECK(parse(serialize(correction), back));
    CHECK(back.hasConfirmed && back.confirmed);
}

// session 0 必須真的出現在線上, 不能被「空值省略」吃掉。
static void testSessionZeroIsEmitted() {
    IpcMessage m;
    m.type = "stop";
    m.setSession(0);
    CHECK_STR_EQ(serialize(m), R"({"type":"stop","session":0})");
}

static void testParseResultChinese() {
    IpcMessage m;
    CHECK(parse(R"({"type":"result","session":42,"text":"幫我看一下 git status"})",
                m));
    CHECK_STR_EQ(m.type, "result");
    CHECK(m.hasSession);
    CHECK_EQ(m.session, 42u);
    CHECK_STR_EQ(m.text, "幫我看一下 git status");
}

static void testParseError() {
    IpcMessage m;
    CHECK(parse(
        R"({"type":"error","session":1,"code":"no_audio_device","text":"找不到麥克風"})",
        m));
    CHECK_STR_EQ(m.code, "no_audio_device");
    CHECK_STR_EQ(m.text, "找不到麥克風");
}

static void testParseNoSession() {
    IpcMessage m;
    CHECK(parse(R"({"type":"pong"})", m));
    CHECK_STR_EQ(m.type, "pong");
    CHECK(!m.hasSession);
}

// 未知欄位必須被忽略而非導致解析失敗——協定要能往前相容。
static void testParseUnknownFieldsIgnored() {
    IpcMessage m;
    CHECK(parse(
        R"({"type":"result","session":3,"text":"hi","future_field":"x","n":1.5,"b":true,"z":null})",
        m));
    CHECK_STR_EQ(m.type, "result");
    CHECK_STR_EQ(m.text, "hi");
    CHECK_EQ(m.session, 3u);
}

// 換行必須能安全地穿過協定。這關乎 SDD §4.5 的終端機安全規則:
// daemon 端要能把含換行的結果送出來讓 addon/daemon 檢查, 前提是
// 換行不會先把 NDJSON 的分行語意打壞。
static void testEscapeRoundTrip() {
    IpcMessage m;
    m.type = "result";
    m.setSession(1);
    m.text = "line1\nline2\ttab \"quoted\" back\\slash\r\n";

    std::string wire = serialize(m);
    CHECK(wire.find('\n') == std::string::npos); // 序列化結果不含裸換行

    IpcMessage back;
    CHECK(parse(wire, back));
    CHECK_STR_EQ(back.text, m.text);
}

static void testControlCharEscape() {
    IpcMessage m;
    m.type = "result";
    m.text = std::string("a\x01\x1f") + "b";
    std::string wire = serialize(m);
    CHECK(wire.find("\\u0001") != std::string::npos);
    CHECK(wire.find("\\u001f") != std::string::npos);

    IpcMessage back;
    CHECK(parse(wire, back));
    CHECK_STR_EQ(back.text, m.text);
}

static void testParseUnicodeEscape() {
    IpcMessage m;
    // 幫 = 幫
    CHECK(parse(R"({"type":"result","text":"幫我"})", m));
    CHECK_STR_EQ(m.text, "幫我");
}

static void testParseSurrogatePair() {
    IpcMessage m;
    // U+1F600 GRINNING FACE
    CHECK(parse(R"({"type":"result","text":"😀"})", m));
    CHECK_STR_EQ(m.text, "\xF0\x9F\x98\x80");
}

static void testParseLoneSurrogate() {
    IpcMessage m;
    CHECK(parse(R"({"type":"result","text":"\ud83d"})", m));
    CHECK_STR_EQ(m.text, "\xEF\xBF\xBD"); // U+FFFD
}

static void testParseMalformed() {
    IpcMessage m;
    CHECK(!parse("", m));
    CHECK(!parse("not json", m));
    CHECK(!parse("{", m));
    CHECK(!parse("{}", m));
    CHECK(!parse(R"({"session":1})", m));          // 缺 type
    CHECK(!parse(R"({"type":"result",)", m));      // 截斷
    CHECK(!parse(R"({"type":"result","text":"unterminated})", m));
    CHECK(!parse(R"({"type":"x","nested":{"a":1}})", m)); // 巢狀不在協定內
}

// 型別不符的欄位應該被忽略, 而不是誤填。
static void testParseTypeMismatch() {
    IpcMessage m;
    CHECK(parse(R"({"type":"result","session":"42"})", m));
    CHECK(!m.hasSession);
}

static void testWhitespaceTolerance() {
    IpcMessage m;
    CHECK(parse("  { \"type\" : \"pong\" }  ", m));
    CHECK_STR_EQ(m.type, "pong");
}

int main() {
    testSerializeStart();
    testContextAndCorrectionWire();
    testSerializeStop();
    testSessionZeroIsEmitted();
    testParseResultChinese();
    testParseError();
    testParseNoSession();
    testParseUnknownFieldsIgnored();
    testEscapeRoundTrip();
    testControlCharEscape();
    testParseUnicodeEscape();
    testParseSurrogatePair();
    testParseLoneSurrogate();
    testParseMalformed();
    testParseTypeMismatch();
    testWhitespaceTolerance();

    if (g_failures) {
        std::fprintf(stderr, "\n%d check(s) failed\n", g_failures);
        return 1;
    }
    std::printf("all ipc tests passed\n");
    return 0;
}
