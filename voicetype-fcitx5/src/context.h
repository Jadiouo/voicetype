#ifndef VOICETYPE_CONTEXT_H
#define VOICETYPE_CONTEXT_H

#include <cstdint>
#include <optional>
#include <string>

namespace voicetype {

// Fcitx offsets count Unicode characters, not UTF-8 bytes.
struct TextSnapshot {
    std::string text;
    size_t cursor = 0;
    size_t anchor = 0;
};

struct ContextText {
    std::string text;
    std::string selection;
};

// Invalid UTF-8 or cursor offsets yield no context. Limits are in characters.
std::optional<ContextText> boundedContext(const TextSnapshot &snapshot);

struct Correction {
    std::string before;
    std::string after;
};

// Only direct ASCII edits have a predictable key-to-text result here. IME,
// clipboard, word deletion and unknown grapheme behavior require confirmation.
struct EditIntent {
    enum class Kind { Insert, Backspace, Delete } kind;
    char ascii = 0;
};

// Tracks exactly one insertion. The application must first acknowledge its exact
// content; we never treat a predicted commit as proof that it reached the app.
// Call clear() on focus/reset/sensitivity changes and before another dictation.
class CorrectionTracker {
public:
    bool begin(const TextSnapshot &before, const std::string &inserted,
               uint64_t nowUsec);
    void observe(const TextSnapshot &snapshot, uint64_t nowUsec);
    void noteUserEditKey(EditIntent intent, const TextSnapshot &before, uint64_t nowUsec);
    void noteSubmitKey(uint64_t nowUsec);
    std::optional<Correction> correction(const TextSnapshot &current,
                                         uint64_t nowUsec) const;
    // The app may clear a sent message before the next dictation. Return only
    // the previously observed, stable user edit, never the clear itself. Call
    // before observe(); a pending editing key means deletion, not submission.
    std::optional<Correction> correctionBeforeClear(const TextSnapshot &current,
                                                   uint64_t nowUsec) const;
    void clear();

private:
    std::optional<Correction> correctionImpl(const TextSnapshot &current,
                                             uint64_t nowUsec, bool requireStable) const;
    std::string prefix_, inserted_, suffix_, expected_, observed_;
    std::optional<TextSnapshot> expectedEdit_, splitDeletion_;
    size_t observedCursor_ = 0;
    size_t observedAnchor_ = 0;
    bool active_ = false;
    bool acknowledged_ = false;
    bool userEdited_ = false;

    uint64_t startedAt_ = 0;
    uint64_t changedAt_ = 0;
    uint64_t userKeyAt_ = 0;
    uint64_t submitKeyAt_ = 0;
};

} // namespace voicetype
#endif
