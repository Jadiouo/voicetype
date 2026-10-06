#include "context.h"

#include <fcitx-utils/utf8.h>

#include <algorithm>
#include <vector>

namespace voicetype {
namespace {
constexpr size_t kMaxContextChars = 2048;
constexpr size_t kMaxSelectionChars = 512;
constexpr size_t kMaxTrackedChars = 4096;
constexpr uint64_t kStableUsec = 350000;
constexpr uint64_t kExpiryUsec = 5 * 60 * 1000000ULL;

std::optional<std::vector<size_t>> offsets(const std::string &text) {
    if (!fcitx::utf8::validate(text)) {
        return std::nullopt;
    }
    std::vector<size_t> result;
    for (size_t i = 0; i < text.size(); ++i) {
        if ((static_cast<unsigned char>(text[i]) & 0xc0) != 0x80) {
            result.push_back(i);
        }
    }
    result.push_back(text.size());
    return result;
}

std::optional<std::vector<size_t>> offsets(const TextSnapshot &snapshot) {
    auto result = offsets(snapshot.text);
    if (!result || snapshot.cursor >= result->size() ||
        snapshot.anchor >= result->size()) {
        return std::nullopt;
    }
    return result;
}

std::string slice(const std::string &text, const std::vector<size_t> &off,
                  size_t start, size_t end) {
    return text.substr(off[start], off[end] - off[start]);
}

bool asciiWord(const std::string &text, size_t index) {
    if (index >= text.size()) {
        return false;
    }
    const unsigned char c = text[index];
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
           (c >= '0' && c <= '9') || c == '_' || c == '-';
}

// Only a small, anchored replacement can be inferred automatically. Pure
// additions/deletions and broad rewrites require the explicit learn shortcut.
bool plausibleReplacement(const std::string &before, const std::string &after,
                          const std::string &outerPrefix,
                          const std::string &outerSuffix) {
    auto b = offsets(before);
    auto a = offsets(after);
    if (!b || !a || before == after || before.empty() || after.empty() ||
        a->size() > kMaxSelectionChars + 1) {
        return false;
    }
    const size_t bn = b->size() - 1;
    const size_t an = a->size() - 1;
    size_t left = 0;
    while (left < std::min(bn, an) &&
           slice(before, *b, left, left + 1) ==
               slice(after, *a, left, left + 1)) {
        ++left;
    }
    size_t right = 0;
    while (right < std::min(bn, an) - left &&
           slice(before, *b, bn - right - 1, bn - right) ==
               slice(after, *a, an - right - 1, an - right)) {
        ++right;
    }
    size_t bend = bn - right;
    size_t aend = an - right;

    // A missing ASCII letter (mabe -> maybe) is a word replacement, while
    // typing another word or sentence is not. Expand only within one word.
    if (left == aend) {
        return false; // never learn a backspace-only edit, even inside a word
    }
    if (left == bend) {
        if (left == 0 || bend == bn || aend == an ||
            !asciiWord(before, (*b)[left - 1]) ||
            !asciiWord(before, (*b)[bend]) ||
            !asciiWord(after, (*a)[left - 1]) ||
            !asciiWord(after, (*a)[aend])) {
            return false;
        }
        const auto gap = slice(after, *a, left, aend);
        if (gap.size() > 2 ||
            !std::all_of(gap.begin(), gap.end(), [](unsigned char c) {
                return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
            })) {
            return false;
        }
        while (left > 0 && asciiWord(before, (*b)[left - 1]) &&
               asciiWord(after, (*a)[left - 1])) {
            --left;
        }
        while (bend < bn && aend < an && asciiWord(before, (*b)[bend]) &&
               asciiWord(after, (*a)[aend])) {
            ++bend;
            ++aend;
        }
        right = bn - bend;
    }
    const size_t removed = bend - left;
    const size_t added = aend - left;
    if (!removed || !added || removed > 32 || added > 32 ||
        std::max(removed, added) > std::min(removed, added) + 3) {
        return false;
    }
    // Keep enough unchanged text to anchor a unique location. Start/end of the
    // field can stand in for one anchor, but an entirely replaced field cannot.
    const auto leftAnchor = outerPrefix + slice(before, *b, 0, left);
    const auto rightAnchor = slice(before, *b, bend, bn) + outerSuffix;
    const size_t lc = fcitx::utf8::length(leftAnchor);
    const size_t rc = fcitx::utf8::length(rightAnchor);
    if (lc + rc < 8 || (lc < 3 && rc < 8) || (rc < 3 && lc < 8)) {
        return false;
    }
    // Repeated unchanged anchors make attribution uncertain, so skip learning.
    const auto expected = outerPrefix + before + outerSuffix;
    const auto unique = [&expected](const std::string &anchor) {
        if (anchor.empty()) {
            return true;
        }
        const auto first = expected.find(anchor);
        return first != std::string::npos &&
               expected.find(anchor, first + 1) == std::string::npos;
    };
    return unique(leftAnchor) && unique(rightAnchor);
}
} // namespace

std::optional<ContextText> boundedContext(const TextSnapshot &snapshot) {
    const auto off = offsets(snapshot);
    if (!off) {
        return std::nullopt;
    }
    const size_t size = off->size() - 1;
    size_t start = snapshot.cursor > kMaxContextChars / 2
                       ? snapshot.cursor - kMaxContextChars / 2
                       : 0;
    const size_t end = std::min(size, start + kMaxContextChars);
    start = end > kMaxContextChars ? end - kMaxContextChars : 0;
    ContextText result{slice(snapshot.text, *off, start, end), {}};
    const size_t selectionStart = std::min(snapshot.cursor, snapshot.anchor);
    const size_t selectionEnd = std::max(snapshot.cursor, snapshot.anchor);
    if (selectionEnd - selectionStart <= kMaxSelectionChars) {
        result.selection = slice(snapshot.text, *off, selectionStart, selectionEnd);
    }
    return result;
}

bool CorrectionTracker::begin(const TextSnapshot &before,
                              const std::string &inserted, uint64_t nowUsec) {
    clear();
    const auto off = offsets(before);
    const auto ins = offsets(inserted);
    if (!off || !ins || before.cursor != before.anchor || inserted.empty() ||
        ins->size() > kMaxSelectionChars + 1 ||
        off->size() + ins->size() > kMaxTrackedChars + 2) {
        return false;
    }
    prefix_ = before.text.substr(0, (*off)[before.cursor]);
    suffix_ = before.text.substr((*off)[before.cursor]);
    inserted_ = inserted;
    expected_ = prefix_ + inserted_ + suffix_;
    startedAt_ = changedAt_ = nowUsec;
    active_ = true;
    return true;
}

void CorrectionTracker::observe(const TextSnapshot &snapshot, uint64_t nowUsec) {
    if (!active_) { return; }
    if (!offsets(snapshot)) { clear(); return; }
    if (!acknowledged_) {
        if (snapshot.text == expected_) {
            acknowledged_ = true;
            observed_ = snapshot.text;
            observedCursor_ = snapshot.cursor;
            observedAnchor_ = snapshot.anchor;
            changedAt_ = nowUsec;
        }
        return;
    }
    const auto matches = [&snapshot](const TextSnapshot &expected) {
        return snapshot.text == expected.text && snapshot.cursor == expected.cursor &&
               snapshot.anchor == expected.anchor;
    };
    if (snapshot.text != observed_ || (expectedEdit_ && matches(*expectedEdit_))) {
        const bool recentKey = expectedEdit_ && userKeyAt_ && nowUsec >= userKeyAt_ &&
                               nowUsec - userKeyAt_ <= 2000000;
        if (recentKey && matches(*expectedEdit_)) {
            userEdited_ = true;
            expectedEdit_.reset(); splitDeletion_.reset(); userKeyAt_ = 0;
        } else if (recentKey && splitDeletion_ && matches(*splitDeletion_)) {
            // One selected literal may arrive as exact deletion then exact
            // insertion. The second update may contain only that one literal.
            splitDeletion_.reset();
        } else {
            clear();
            return;
        }
        observed_ = snapshot.text;
        changedAt_ = nowUsec;
        submitKeyAt_ = 0;
    } else if (expectedEdit_ &&
               (snapshot.cursor != observedCursor_ || snapshot.anchor != observedAnchor_)) {
        // A caret move before the expected edit prevents attributing it later.
        clear();
        return;
    }
    observedCursor_ = snapshot.cursor;
    observedAnchor_ = snapshot.anchor;
}

void CorrectionTracker::noteUserEditKey(EditIntent intent, const TextSnapshot &before,
                                        uint64_t nowUsec) {
    if (!active_ || !acknowledged_) { return; }
    const auto off = offsets(before);
    // Do not infer a batch from several keys when the first has no matching ack.
    if (expectedEdit_ || !off || before.text != observed_) { clear(); return; }
    size_t start = std::min(before.cursor, before.anchor);
    size_t end = std::max(before.cursor, before.anchor);
    const bool selected = start != end;
    const bool insertion = intent.kind == EditIntent::Kind::Insert;
    if (insertion && (intent.ascii < 0x20 || intent.ascii > 0x7e)) { clear(); return; }
    if (!insertion && !selected) {
        if (intent.kind == EditIntent::Kind::Backspace && start > 0) { --start; }
        else if (intent.kind == EditIntent::Kind::Delete && end + 1 < off->size()) { ++end; }
        else { clear(); return; }
    }
    const auto removed = slice(before.text, *off, start, end);
    if (!std::all_of(removed.begin(), removed.end(), [](unsigned char c) {
            return c >= 0x20 && c <= 0x7e;
        })) { clear(); return; }
    const auto prefix = before.text.substr(0, (*off)[start]);
    const auto suffix = before.text.substr((*off)[end]);
    const std::string inserted = insertion ? std::string(1, intent.ascii) : std::string();
    const auto caret = start + inserted.size();
    expectedEdit_ = TextSnapshot{prefix + inserted + suffix, caret, caret};
    if (selected && insertion) { splitDeletion_ = TextSnapshot{prefix + suffix, start, start}; }
    else { splitDeletion_.reset(); }
    observedCursor_ = before.cursor; observedAnchor_ = before.anchor;
    userKeyAt_ = nowUsec;
    submitKeyAt_ = 0;
}

void CorrectionTracker::noteSubmitKey(uint64_t nowUsec) {
    if (active_ && acknowledged_ && !userKeyAt_) {
        submitKeyAt_ = nowUsec;
    }
}

std::optional<Correction> CorrectionTracker::correction(
    const TextSnapshot &current, uint64_t nowUsec) const {
    return correctionImpl(current, nowUsec, true);
}

std::optional<Correction> CorrectionTracker::correctionImpl(
    const TextSnapshot &current, uint64_t nowUsec, bool requireStable) const {
    if (!active_ || !acknowledged_ || !userEdited_ || userKeyAt_ || !offsets(current) ||
        current.cursor != current.anchor || current.text != observed_ ||
        nowUsec < changedAt_ || (requireStable && nowUsec - changedAt_ < kStableUsec) ||
        nowUsec < startedAt_ || nowUsec - startedAt_ > kExpiryUsec ||
        current.text.size() < prefix_.size() + suffix_.size() ||
        current.text.compare(0, prefix_.size(), prefix_) != 0 ||
        current.text.compare(current.text.size() - suffix_.size(), suffix_.size(),
                             suffix_) != 0) {
        return std::nullopt;
    }
    const auto after = current.text.substr(
        prefix_.size(), current.text.size() - prefix_.size() - suffix_.size());
    if (!plausibleReplacement(inserted_, after, prefix_, suffix_)) {
        return std::nullopt;
    }
    return Correction{inserted_, after};
}

std::optional<Correction> CorrectionTracker::correctionBeforeClear(
    const TextSnapshot &current, uint64_t nowUsec) const {
    if (userKeyAt_ || !offsets(current) || current.cursor != current.anchor ||
        current.text != prefix_ + suffix_ || current.text == observed_) {
        return std::nullopt;
    }
    // Preserve the exact insertion's surrounding anchors, including cursor
    // position; another field value or an unrelated deletion is not a send.
    const auto prefixOffsets = offsets(prefix_);
    if (!prefixOffsets || current.cursor != prefixOffsets->size() - 1) {
        return std::nullopt;
    }
    // Return alone is not confirmation: a newline never reaches this branch.
    // Return AND an exact clear can finish a quick edit without the idle delay.
    const bool submitted = submitKeyAt_ && submitKeyAt_ >= changedAt_ &&
        nowUsec >= submitKeyAt_ && nowUsec - submitKeyAt_ <= 1000000;
    return correctionImpl({observed_, observedCursor_, observedAnchor_}, nowUsec, !submitted);
}

void CorrectionTracker::clear() { *this = CorrectionTracker{}; }
} // namespace voicetype
