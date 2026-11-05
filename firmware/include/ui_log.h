#pragma once
#include "app_state.h"

// Live conversation log for one agent or subagent, chat style, drag to scroll.
namespace LogScreen {
constexpr int X = 56, W = WIDTH - 2 * X - 16, CHIP = 30;

// Lay entries out into positioned lines; consecutive tool calls become chip rows.
inline void layout() {
  LogView &v = logView;
  v.lines.clear();
  int y = 0;
  for (size_t i = 0; i < v.entries.size();) {
    const LogEntry &e = v.entries[i];
    if (e.role == 't') {
      String row; int x = 0;
      while (i < v.entries.size() && v.entries[i].role == 't') {
        String name = canvas.fit(v.entries[i].text, font_s16, 280);
        int w = canvas.pillWidth(name, font_s16, CHIP, true) + 8;
        if (x + w > W && row.length()) { v.lines.push_back({ToolLine, 't', y, row}); y += CHIP + 8; row = ""; x = 0; }
        row += (row.length() ? "\n" : "") + name; x += w; i++;
      }
      if (row.length()) { v.lines.push_back({ToolLine, 't', y, row}); y += CHIP + 8; }
      y += 10;
      continue;
    }
    v.lines.push_back({LabelLine, e.role, y, e.role == 'u' ? String("You") : v.agentLabel});
    y += 26;
    for (auto &line : canvas.wrap(e.text, font_r20, W)) { v.lines.push_back({BodyLine, e.role, y, line}); y += 28; }
    y += 20; i++;
  }
  v.contentHeight = y;
  v.layoutDirty = false;
  v.scroll = v.follow ? logMaxScroll() : min(v.scroll, logMaxScroll());
}

inline void header() {
  using namespace Theme;
  LogView &v = logView;
  canvas.fillRoundRect(MARGIN, 16, 112, 44, 22, RAISED);
  canvas.chevronLeft(MARGIN + 26, 38, 8, TEXT);
  canvas.text(MARGIN + 42, 38 - font_s20.height / 2, "Back", font_s20, TEXT);
  addHit(0, 0, 160, LOG_TOP, Back);

  // Right side: follow state, then the session's current status.
  int right = WIDTH - MARGIN - 8;
  if (!fresh()) {
    right -= canvas.pill(right - canvas.pillWidth("Offline", font_s16, 28, false), 22, "Offline", font_s16, TEXT2, RAISED, 28) + 12;
  } else if (v.follow) {
    int tw = canvas.textWidth("LIVE", font_s16);
    canvas.text(right - tw, 38 - font_s16.height / 2, "LIVE", font_s16, WORKING);
    canvas.fillCircle(right - tw - 12, 38, 9, WORKING, 60);
    canvas.fillCircle(right - tw - 12, 38, 4.5f, WORKING);
    right -= tw + 36;
  } else {
    String label = "Paused - tap to follow";
    int w = canvas.pillWidth(label, font_s16, 32, false);
    canvas.pill(right - w, 20, label, font_s16, TEXT, mix(SURFACE, WAITING, 70), 32);
    addHit(right - w - 6, 10, w + 12, 52, Follow);
    right -= w + 14;
  }
  const Agent *a = findAgent(v.agentId);
  const Child *c = v.childId.length() ? findChild(a, v.childId) : nullptr;
  String status = v.childId.length() ? (c ? c->status : String()) : (a ? a->status : String());
  if (status.length()) {
    StatusStyle st = statusStyle(status);
    int w = canvas.pillWidth(st.label, font_s16, 28, true);
    canvas.pill(right - w, 24, st.label, font_s16, TEXT, st.pill, 28, st.color);
    right -= w + 16;
  } else {
    int w = canvas.pillWidth("ENDED", font_s16, 28, false);
    canvas.pill(right - w, 24, "ENDED", font_s16, TEXT2, RAISED, 28);
    right -= w + 16;
  }
  int tx = MARGIN + 136, avail = right - tx;
  canvas.text(tx, 12, canvas.fit(v.title, font_s20, avail), font_s20, TEXT);
  dotted(tx, 40, canvas.fit(v.contextA, font_s16, avail / 2), canvas.fit(v.contextB, font_s16, avail / 2 - 20), font_s16, TEXT2);
  canvas.fillRect(0, LOG_TOP - 1, WIDTH, 1, DIVIDER);
}

inline void message(const String &title, const String &detail, bool busy) {
  using namespace Theme;
  int cy = LOG_TOP + (HEIGHT - LOG_TOP) / 2;
  if (busy) { canvas.fillCircle(488, cy - 50, 5, DONE); canvas.fillCircle(512, cy - 50, 5, DONE, 160); canvas.fillCircle(536, cy - 50, 5, DONE, 80); }
  canvas.textCenter(512, cy - 14, title, font_s28, TEXT);
  canvas.textCenter(512, cy + 30, detail, font_s20, TEXT2);
}

inline uint32_t key() {
  const LogView &v = logView;
  const Agent *a = findAgent(v.agentId);
  const Child *c = v.childId.length() ? findChild(a, v.childId) : nullptr;
  FrameKey k; k.add(3u);
  k.add(v.revision); k.add(uint32_t(v.scroll)); k.add(uint32_t(v.follow)); k.add(uint32_t(fresh())); k.add(v.state);
  k.add(v.childId.length() ? (c ? c->status : String()) : (a ? a->status : String()));
  return k.h;
}

inline void draw() {
  using namespace Theme;
  LogView &v = logView;
  if (v.layoutDirty) layout();
  canvas.fillScreen(BG);
  header();
  if (v.entries.empty()) {
    if (v.state == "loading") message("Loading conversation...", "Fetching recent messages from the computer.", true);
    else if (v.state == "unavailable") message("Log unavailable", "This session's transcript can't be read on the computer.", false);
    else message("No messages yet", "New messages appear here as the agent works.", false);
    return;
  }
  canvas.clip(0, LOG_TOP, WIDTH, HEIGHT - LOG_TOP);
  int top = LOG_TOP + LOG_PAD - v.scroll;
  for (const LogLine &l : v.lines) {
    int y = top + l.y;
    if (y > HEIGHT || y + 40 < LOG_TOP) continue;
    if (l.kind == LabelLine) {
      uint16_t c = l.role == 'u' ? USER : AGENT;
      canvas.fillCircle(X - 18, y + font_s16.height / 2.f, 4, c);
      canvas.text(X, y, l.text, font_s16, c);
    } else if (l.kind == BodyLine) {
      if (l.role == 'u') canvas.fillRect(X - 20, y - 2, 3, 28, mix(BG, USER, 120));
      canvas.text(X, y + 1, l.text, font_r20, l.role == 'u' ? TEXT : mix(TEXT2, TEXT, 170));
    } else {
      int x = X, start = 0;
      while (start <= int(l.text.length())) {
        int end = l.text.indexOf('\n', start); if (end < 0) end = l.text.length();
        x += canvas.pill(x, y, l.text.substring(start, end), font_s16, TEXT2, RAISED, CHIP, TEXT3) + 8;
        start = end + 1;
      }
    }
  }
  int maxScroll = logMaxScroll();
  if (maxScroll > 0) {
    int viewH = logViewHeight(), total = v.contentHeight + 2 * LOG_PAD;
    int thumb = max(40, viewH * viewH / total);
    int ty = LOG_TOP + int(int64_t(viewH - thumb) * v.scroll / maxScroll);
    canvas.fillRoundRect(WIDTH - 12, ty + 4, 5, thumb - 8, 3, DIVIDER);
  }
  canvas.unclip();
  if (!v.follow && v.scroll < maxScroll - 4) {
    String label = "Jump to latest";
    int w = canvas.textWidth(label, font_s16) + 64, x = WIDTH / 2 - w / 2, y = HEIGHT - 64;
    canvas.fillRoundRect(x, y, w, 40, 20, RAISED);
    canvas.text(x + 22, y + (40 - font_s16.height) / 2, label, font_s16, TEXT);
    canvas.chevronDown(x + w - 26, y + 20, 6, TEXT);
    addHit(x - 8, y - 8, w + 16, 56, Follow);
  }
}
}
