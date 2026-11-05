#pragma once
#include "app_state.h"

// Main screen: summary bar plus one tinted card per agent session.
namespace GridScreen {
constexpr int BAR = 56;
inline String clockText();

inline void topBar() {
  using namespace Theme;
  canvas.logo(36, BAR / 2, 8);
  canvas.text(58, (BAR - font_s20.height) / 2, "agentlight", font_s20, TEXT);

  // Status summary, centered: one colored dot + count per status present.
  const char *keys[] = {"waiting", "error", "working", "idle"};
  const char *names[] = {"need you", "error", "working", "idle"};
  String labels[4]; uint16_t colors[4]; int n = 0, total = 0;
  for (int k = 0; k < 4; k++) {
    int count = 0;
    for (int i = 0; i < agentCount; i++) if (agents[i].status == keys[k]) count++;
    if (!count) continue;
    labels[n] = String(count) + " " + (k == 0 && count == 1 ? "needs you" : names[k]);
    colors[n] = statusStyle(keys[k]).color;
    total += 16 + canvas.textWidth(labels[n], font_s16) + (n ? 28 : 0);
    n++;
  }
  int x = WIDTH / 2 - total / 2;
  for (int i = 0; i < n; i++) {
    if (i) x += 28;
    canvas.fillCircle(x + 5, BAR / 2.f, 5, colors[i]);
    canvas.text(x + 16, (BAR - font_s16.height) / 2, labels[i], font_s16, TEXT2);
    x += 16 + canvas.textWidth(labels[i], font_s16);
  }

  // Local clock from the helper's time, plus the transport in use.
  String clock = clockText();
  int right = WIDTH - MARGIN - 8;
  canvas.textRight(right, (BAR - font_s20.height) / 2, clock, font_s20, TEXT);
  int iconX = right - canvas.textWidth(clock, font_s20) - 26;
  if (usingWifi) canvas.wifiIcon(iconX, BAR / 2 + 8, 16, TEXT2);
  else canvas.usbIcon(iconX, BAR / 2, 18, TEXT2);

  // Sleep control: tap to activate; during an alert, tap to go back to sleep.
  int sleepRight = iconX - 22;
  if (power.alerting()) {
    int w = canvas.pillWidth("Sleep", font_s16, 32, false);
    canvas.pill(sleepRight - w, (BAR - 32) / 2, "Sleep", font_s16, BG, WAITING, 32);
    addHit(sleepRight - w - 8, 0, w + 16, BAR, ToggleSleep);
  } else {
    int cx = sleepRight - 18;
    if (power.active()) { canvas.fillCircle(cx, BAR / 2, 18, RAISED); canvas.moon(cx, BAR / 2, 10, DONE, RAISED); }
    else canvas.moon(cx, BAR / 2, 10, TEXT3, BG);
    addHit(cx - 28, 0, 56, BAR, ToggleSleep);
  }
  canvas.fillRect(0, BAR, WIDTH, 1, DIVIDER);
}

inline void card(int index, int x, int y, int w, int h) {
  using namespace Theme;
  const Agent &a = agents[index];
  StatusStyle st = statusStyle(a.status);
  // Accent edge: only a narrow strip is painted twice, not the whole card.
  canvas.fillRoundRect(x, y, RADIUS * 2, h, RADIUS, st.color);
  canvas.fillRoundRect(x + 6, y, w - 6, h, RADIUS, st.tint);
  addHit(x, y, w, h, OpenAgent, index);

  bool compact = h < 200 || w < 340;
  int pad = compact ? 14 : 20, ix = x + 6 + pad, iw = w - 6 - 2 * pad, cy = y + pad;
  canvas.pill(ix, cy, st.label, font_s16, TEXT, st.pill, 28, st.color);
  canvas.textRight(x + w - pad, cy + (28 - font_s16.height) / 2, duration(elapsed(a.age)), font_s16, TEXT2);
  cy += 28 + (compact ? 8 : 14);

  // Footer, measured bottom-up: subagent pills, then context and task bars.
  int bottom = y + h - pad;
  int footer = (a.childCount ? 40 : 0) + (a.tasksTotal ? 24 : 0) + (a.limit ? 24 : 0);
  int subtitle = font_s16.height + 4;
  int room = bottom - footer - subtitle - cy;

  String title = a.activity.length() ? a.activity : a.name;
  const BitmapFont *fonts[] = {&font_s40, &font_s28, &font_s20};
  std::vector<String> lines; const BitmapFont *font = &font_s20;
  for (int i = compact ? 1 : 0; i < 3; i++) {
    int maxLines = max(1, min(2, room / fonts[i]->height));
    lines = canvas.wrap(title, *fonts[i], iw, maxLines);
    font = fonts[i];
    bool cut = !lines.empty() && lines.back().endsWith("...");
    if (int(lines.size()) * fonts[i]->height <= room && !cut) break;
  }
  for (auto &line : lines) { canvas.text(ix, cy, line, *font, TEXT); cy += font->height; }
  dotted(ix, cy + 2, a.activity.length() ? canvas.fit(a.name, font_s16, iw / 2) : "", kindLabel(a.kind), font_s16, TEXT2);

  int fy = bottom;
  if (a.childCount) {
    fy -= 32;
    int px = ix;
    uint16_t bg = mix(st.tint, BG, 120);
    for (int j = 0; j < a.childCount; j++) {
      const Child &c = a.children[j];
      int left = a.childCount - j;
      String more = "+" + String(left);
      int reserve = left > 1 ? canvas.pillWidth(more, font_s16, 32, false) + 8 : 0;
      String name = canvas.fit(c.name, font_s16, min(220, ix + iw - px - reserve - 40));
      int pw = canvas.pillWidth(name, font_s16, 32, true);
      if (px + pw > ix + iw - reserve || name == "...") {
        canvas.pill(px, fy, more, font_s16, TEXT2, bg, 32);
        break;
      }
      canvas.pill(px, fy, name, font_s16, TEXT, bg, 32, statusStyle(c.status).color);
      addHit(px - 4, fy - 6, pw + 8, 44, OpenChild, index, j);
      px += pw + 8;
    }
    fy -= 8;
  }
  uint16_t track = mix(st.tint, BG, 150);
  auto bar = [&](const String &label, float frac, uint16_t color) {
    fy -= 24;
    int lw = canvas.textWidth(label, font_s16);
    canvas.text(ix, fy, label, font_s16, TEXT2);
    canvas.progress(ix + lw + 12, fy + 7, iw - lw - 12, 6, frac, color, track);
  };
  if (a.limit) {
    float frac = float(min(a.used, a.limit)) / a.limit;
    bar("Context " + String(int(frac * 100 + 0.5f)) + "%", frac, frac > 0.85f ? WAITING : TEXT2);
  }
  if (a.tasksTotal) bar(String(a.tasksDone) + "/" + String(a.tasksTotal) + " tasks", float(a.tasksDone) / a.tasksTotal, st.color);
}

inline void emptyState() {
  using namespace Theme;
  canvas.logo(512, 240, 18);
  canvas.textCenter(512, 280, "No active agent sessions", font_s28, TEXT);
  canvas.textCenter(512, 326, "Start Claude Code, Codex or OpenCode and it will appear here.", font_s20, TEXT2);
}

inline String clockText() {
  uint64_t local = hostTime + tzOffset + (millis() - receivedAt) / 1000;
  char clock[6]; snprintf(clock, sizeof(clock), "%02u:%02u", unsigned(local / 3600 % 24), unsigned(local / 60 % 60));
  return clock;
}
inline uint32_t key() {
  FrameKey k; k.add(1u);
  k.add(uint32_t(power.active())); k.add(uint32_t(power.alerting()));
  k.add(uint32_t(page)); k.add(uint32_t(agentCount)); k.add(uint32_t(moreAgents)); k.add(uint32_t(usingWifi)); k.add(clockText());
  for (int i = page * PAGE_SIZE; i < min(agentCount, (page + 1) * PAGE_SIZE); i++) {
    const Agent &a = agents[i];
    k.add(a.status); k.add(a.activity); k.add(a.name); k.add(a.kind); k.add(duration(elapsed(a.age)));
    k.add(a.tasksDone); k.add(a.tasksTotal); k.add(a.limit ? a.used * 100 / a.limit : 0);
    for (int j = 0; j < a.childCount; j++) { k.add(a.children[j].name); k.add(a.children[j].status); }
  }
  for (int i = 0; i < agentCount; i++) k.add(agents[i].status);  // top bar counts
  return k.h;
}

inline void draw() {
  using namespace Theme;
  canvas.fillScreen(BG);
  topBar();
  int pages = pageCount();
  if (page >= pages) page = 0;
  if (!agentCount) { emptyState(); return; }

  int visible = min(PAGE_SIZE, agentCount - page * PAGE_SIZE);
  int columns = visible <= 1 ? 1 : visible <= 4 ? 2 : 3;
  int rows = (visible + columns - 1) / columns;
  int top = BAR + MARGIN, bottom = HEIGHT - (pages > 1 ? 40 : MARGIN);
  int cardH = (bottom - top - (rows - 1) * GAP) / rows;
  for (int i = 0; i < visible; i++) {
    int row = i / columns, inRow = min(columns, visible - row * columns);
    int cardW = (WIDTH - 2 * MARGIN - (inRow - 1) * GAP) / inRow;
    card(page * PAGE_SIZE + i, MARGIN + (i % columns) * (cardW + GAP), top + row * (cardH + GAP), cardW, cardH);
  }
  if (pages > 1) {
    int dotsW = (pages - 1) * 20 + 28, x = WIDTH / 2 - dotsW / 2, y = HEIGHT - 20;
    for (int p = 0; p < pages; p++) {
      if (p == page) { canvas.fillRoundRect(x, y - 4, 28, 8, 4, TEXT); x += 40; }
      else { canvas.fillCircle(x + 4, y, 4, TEXT3); x += 20; }
    }
    if (moreAgents) canvas.textRight(WIDTH - MARGIN - 8, HEIGHT - 31, "+" + String(moreAgents) + " more", font_s16, TEXT3);
    addHit(0, HEIGHT - 40, WIDTH / 2, 40, PrevPage);
    addHit(WIDTH / 2, HEIGHT - 40, WIDTH / 2, 40, NextPage);
  }
}
}
