#pragma once
#include "app_state.h"
#include "ota.h"

// Full-screen firmware update view; replaces every other screen while active.
namespace OtaScreen {
inline String sizeText(uint32_t bytes) {
  if (bytes < 1024 * 1024) return String((bytes + 512) / 1024) + " KB";
  char t[16]; snprintf(t, sizeof(t), "%.1f MB", bytes / 1048576.0f); return t;
}

inline uint32_t key() {
  FrameKey k; k.add(4u);
  uint32_t total = max<uint32_t>(1, Ota::total);
  uint32_t done = Ota::phase == Ota::Installing ? uint32_t(Ota::written) : uint32_t(Ota::received);
  k.add(uint32_t(Ota::phase)); k.add(uint32_t(uint64_t(done) * 100 / total));
  k.add((millis() - Ota::startedMs) / 2000);  // refresh the time-left estimate every 2 s
  return k.h;
}

inline void draw() {
  using namespace Theme;
  static uint32_t rateAt = 0, rateBytes = 0; static float rate = 0;
  uint8_t phase = Ota::phase;
  uint32_t got = Ota::received, total = max<uint32_t>(1, Ota::total), now = millis();
  bool failed = phase == Ota::Failed;
  canvas.fillScreen(BG);
  canvas.logo(512, 112, 14);
  canvas.textCenter(512, 144, failed ? "Update failed" : phase == Ota::Restarting ? "Update installed" : "Updating firmware", font_s40, TEXT);
  String to = Ota::newVersion[0] ? String("v") + Ota::newVersion : String("new build");
  String versions = String("v") + FW_VERSION + "   ->   " + to;
  canvas.textCenter(512, 202, versions, font_s20, TEXT2);

  constexpr int BX = 212, BY = 340, BW = 600, BH = 14;
  uint16_t accent = failed ? ERROR_ : WORKING;
  canvas.fillRoundRect(BX, BY, BW, BH, BH / 2, RAISED);
  int pct = int(uint64_t(got) * 100 / total);
  String headline, detail;
  if (phase == Ota::Preparing) {
    headline = "Preparing"; detail = "Getting ready for the new firmware...";
    rateAt = 0; rate = 0;
  } else {
    uint32_t done = phase == Ota::Installing ? uint32_t(Ota::written) : got;
    float frac = phase == Ota::Downloading || phase == Ota::Installing || failed ? float(done) / total : 1.f;
    canvas.progress(BX, BY, BW, BH, frac, phase == Ota::Installing ? DONE : accent, RAISED);
    if (phase == Ota::Downloading) {
      // Exponentially smoothed throughput, sampled every ~500 ms.
      if (!rateAt) { rateAt = now; rateBytes = got; }
      else if (now - rateAt >= 500) {
        float sample = (got - rateBytes) * 1000.0f / (now - rateAt);
        rate = rate > 0 ? rate * 0.7f + sample * 0.3f : sample; rateAt = now; rateBytes = got;
      }
      headline = String(pct) + "%";
      detail = "Downloading  " + sizeText(got) + " of " + sizeText(total);
      if (now - Ota::startedMs < 2000 || rate <= 0) detail += "  -  calculating time left...";
      else {
        uint32_t left = uint32_t((total - got) / rate) + 1;
        detail += "  -  about " + (left < 60 ? String(left) + " s" : String((left + 30) / 60) + " min") + " left";
      }
    } else if (phase == Ota::Verifying) { headline = "100%"; detail = "Checking the update..."; }
    else if (phase == Ota::Installing) {
      headline = String(int(uint64_t(done) * 100 / total)) + "%";
      detail = "Installing - the screen may flicker for a moment";
    } else if (phase == Ota::Restarting) { headline = "Done"; detail = "Restarting with the new firmware..."; }
    else { headline = String(pct) + "%"; detail = canvas.fit(String("Still running v") + FW_VERSION + "  (" + Ota::error + ")", font_s20, 800); }
  }
  canvas.textCenter(512, BY - font_s56.height - 18, headline, font_s56, TEXT);
  canvas.textCenter(512, BY + BH + 24, detail, font_s20, failed ? TEXT : TEXT2);
  canvas.textCenter(512, HEIGHT - 56, failed ? "Nothing was changed. The helper can retry later." : "Keep the display powered on", font_s16, TEXT3);
}
}
