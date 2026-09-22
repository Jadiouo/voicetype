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

// Tracks exactly one insertion. The application must first acknowledge its exact
// content; we never treat a predicted commit as proof that it reached the app.
// Call clear() on focus/reset/sensitivity changes and before another dictation.
class CorrectionTracker {
public:
    bool begin(const TextSnapshot &before, const std::string &inserted,
               uint64_t nowUsec);
    void observe(const TextSnapshot &snapshot, uint64_t nowUsec);
    void noteUserEditKey(uint64_t nowUsec);
    std::optional<Correction> correction(const TextSnapshot &current,
                                         uint64_t nowUsec) const;
    void clear();

private:
    std::string prefix_, inserted_, suffix_, expected_, observed_;
    bool active_ = false;
    bool acknowledged_ = false;
    bool userEdited_ = false;
    uint64_t startedAt_ = 0;
    uint64_t changedAt_ = 0;
    uint64_t userKeyAt_ = 0;
};

} // namespace voicetype
#endif
