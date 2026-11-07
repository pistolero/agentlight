#pragma once
#include <Arduino.h>
#include <atomic>
#include <ArduinoJson.h>
#include <WiFi.h>
#include <NetworkClientSecure.h>
#include <ESPmDNS.h>
#include <Preferences.h>
#include <esp_mac.h>
#include <Update.h>
#include <mbedtls/sha256.h>
#include "ota.h"

// Single bounded protocol for USB and TLS. Provisioning is accepted only by
// the main UI's USB parser; network peers cannot alter credentials.
namespace WifiLink {
struct Frame { char text[4097]; };
struct Config { String ssid,password,helper,fingerprint,token; };
static Config settings;
static SemaphoreHandle_t mutex;
static QueueHandle_t incoming,outgoing;
static unsigned revision=0;
// Connection progress for the boot screen (written by the Wi-Fi task).
enum LinkState : uint8_t { NoConfig, Joining, Searching, Connecting, Authorized };
static std::atomic<uint8_t> linkState{NoConfig};
static String deviceId() {
  uint8_t mac[6];esp_read_mac(mac,ESP_MAC_WIFI_STA);
  char text[18];snprintf(text,sizeof(text),"%02X:%02X:%02X:%02X:%02X:%02X",mac[0],mac[1],mac[2],mac[3],mac[4],mac[5]);return text;
}
static bool validHex(const String &s, int length) {
  if(s.length()!=unsigned(length)) return false;
  for(unsigned i=0;i<s.length();i++) if(!isxdigit(s[i])) return false;
  return true;
}
static bool configure(JsonObjectConst root) {
  if(!mutex) return false;
  Config c;
  c.ssid=root["ssid"] | "";c.password=root["password"] | "";
  c.helper=root["helper"] | "";c.fingerprint=root["fingerprint"] | "";c.token=root["token"] | "";
  if(!c.ssid.length() || c.ssid.length()>32 || c.password.length()>63 || !validHex(c.helper,16) || !validHex(c.fingerprint,64) || !validHex(c.token,64)) return false;
  JsonDocument doc;doc["ssid"]=c.ssid;doc["password"]=c.password;doc["helper"]=c.helper;doc["fingerprint"]=c.fingerprint;doc["token"]=c.token;
  String blob;serializeJson(doc,blob);
  Preferences prefs;if(!prefs.begin("agentlight",false)) return false;
  bool ok=prefs.putString("wifi",blob)>0;prefs.end();
  if(!ok) return false;
  xSemaphoreTake(mutex,portMAX_DELAY);settings=c;revision++;xSemaphoreGive(mutex);return true;
}
static String ssid() {
  if(!mutex) return "";
  xSemaphoreTake(mutex,portMAX_DELAY);String s=settings.ssid;xSemaphoreGive(mutex);return s;
}
static bool send(const String &text) {
  if(!outgoing || text.length()>4096) return false;
  static Frame f;text.toCharArray(f.text,sizeof(f.text));return xQueueSend(outgoing,&f,0)==pdTRUE;
}
static bool receive(Frame &frame) {return incoming && xQueueReceive(incoming,&frame,0)==pdTRUE;}
static void reply(NetworkClientSecure &client,const JsonDocument &doc) {serializeJson(doc,client);client.println();}
static void otaResult(NetworkClientSecure &client,bool ok,const char *error) {
  JsonDocument r;r["v"]=1;r["t"]="ota_result";r["ok"]=ok;if(!ok) r["error"]=error;reply(client,r);
}
// Receive exactly `size` raw bytes after ota_begin into PSRAM, verify SHA-256,
// then write flash in one short burst. Flash writes stall the RGB panel's PSRAM
// reads, so keeping them out of the download keeps the progress screen steady.
// The boot partition switches only if everything checks out. Blocks the Wi-Fi task.
static void runOta(NetworkClientSecure &client,uint32_t size,const String &sha,const String &version) {
  strlcpy(Ota::newVersion,version.c_str(),sizeof(Ota::newVersion));
  Ota::received=0;Ota::written=0;Ota::total=size;Ota::startedMs=millis();Ota::setPhase(Ota::Preparing);
  const esp_partition_t *target=esp_ota_get_next_update_partition(nullptr);
  uint8_t *image=nullptr;
  auto abortWith=[&](const char *reason){
    if(Update.isRunning()) Update.abort();
    free(image);image=nullptr;
    Ota::fail(reason);otaResult(client,false,reason);client.flush();client.stop();
  };
  if(!size || !target || size>target->size) {abortWith("image does not fit");return;}
  if(!validHex(sha,64)) {abortWith("bad checksum");return;}
  image=static_cast<uint8_t *>(ps_malloc(size));
  if(!image) {abortWith("not enough memory");return;}
  {JsonDocument r;r["v"]=1;r["t"]="ota_ready";reply(client,r);}
  uint32_t got=0,lastData=millis();
  Ota::startedMs=millis();Ota::setPhase(Ota::Downloading);
  while(got<size) {
    if(!client.connected() && !client.available()) {abortWith("connection lost");return;}
    int n=client.read(image+got,min<uint32_t>(4096,size-got));
    if(n<=0) {
      if(millis()-lastData>15000) {abortWith("transfer stalled");return;}
      vTaskDelay(pdMS_TO_TICKS(5));continue;
    }
    lastData=millis();got+=n;Ota::received=got;
  }
  Ota::setPhase(Ota::Verifying);
  uint8_t digest[32];
  mbedtls_sha256_context ctx;mbedtls_sha256_init(&ctx);mbedtls_sha256_starts(&ctx,0);
  mbedtls_sha256_update(&ctx,image,size);mbedtls_sha256_finish(&ctx,digest);mbedtls_sha256_free(&ctx);
  uint8_t diff=0;
  for(int j=0;j<32;j++) {char pair[3]={sha[2*j],sha[2*j+1],0};diff|=digest[j]^strtoul(pair,nullptr,16);}
  if(diff) {abortWith("checksum mismatch");return;}
  // Flash writes block the panel's PSRAM reads and make the picture jump, so the
  // UI shows a notice and turns the backlight off first. Never wait on it forever.
  Ota::flashGo=false;Ota::setPhase(Ota::Installing);
  for(uint32_t t=millis();!Ota::flashGo && millis()-t<4000;) vTaskDelay(pdMS_TO_TICKS(20));
  if(!Update.begin(size,U_FLASH)) {abortWith("cannot prepare flash");return;}
  for(uint32_t off=0;off<size;) {
    size_t n=min<uint32_t>(4096,size-off);
    if(Update.write(image+off,n)!=n) {abortWith("flash write failed");return;}
    off+=n;Ota::written=off;
  }
  free(image);image=nullptr;
  if(!Update.end(true)) {abortWith("image rejected");return;}
  otaResult(client,true,"");client.flush();
  Ota::setPhase(Ota::Restarting);
  vTaskDelay(pdMS_TO_TICKS(2500));
  client.stop();ESP.restart();
}
static void task(void *) {
  NetworkClientSecure client;
  Config cfg;unsigned current=~0u;
  bool mdns=false,authorized=false;
  uint32_t attempted=0,lastMessage=0;size_t len=0;
  static Frame rx,tx;
  WiFi.persistent(false);
  WiFi.setAutoReconnect(true);
  for(;;) {
    xSemaphoreTake(mutex,portMAX_DELAY);
    if(current!=revision) {cfg=settings;current=revision;client.stop();authorized=false;attempted=0;WiFi.disconnect();if(cfg.ssid.length()){WiFi.mode(WIFI_STA);WiFi.begin(cfg.ssid.c_str(),cfg.password.c_str());}}
    xSemaphoreGive(mutex);
    if(!cfg.ssid.length() || WiFi.status()!=WL_CONNECTED) {
      linkState=cfg.ssid.length() ? Joining : NoConfig;client.stop();authorized=false;
      vTaskDelay(pdMS_TO_TICKS(250));continue;
    }
    if(!mdns) {String hostname="agentlight-display-"+deviceId();hostname.replace(":","");mdns=MDNS.begin(hostname.c_str());}
    if(!client.connected()) {
      authorized=false;len=0;linkState=Searching;
      if(attempted && millis()-attempted<10000) {vTaskDelay(pdMS_TO_TICKS(100));continue;}
      attempted=millis();
      int count=MDNS.queryService("agentlight","tcp");
      for(int i=0;i<count;i++) {
        if(MDNS.txt(i,"helper")!=cfg.helper) continue;
        // TLS is verified by an exact SHA-256 certificate pin provisioned over
        // USB, not by the unauthenticated mDNS records. Send NO credentials or
        // application data until the pin matches. This avoids an NTP dependency.
        client.setInsecure();client.setHandshakeTimeout(5);client.setTimeout(1000);
        linkState=Connecting;
        if(!client.connect(MDNS.address(i),MDNS.port(i),3000)) {linkState=Searching;continue;}
        uint8_t fingerprint[32];bool match=client.getFingerprintSHA256(fingerprint);
        uint8_t diff=0;
        for(int j=0;j<32 && match;j++) {char pair[3]={cfg.fingerprint[2*j],cfg.fingerprint[2*j+1],0};diff|=fingerprint[j]^strtoul(pair,nullptr,16);}
        if(!match || diff) {client.stop();continue;}
        JsonDocument auth;auth["v"]=1;auth["t"]="auth";auth["device"]=deviceId();auth["token"]=cfg.token;
        auth["ota"]=1;auth["fw"]=FW_VERSION;auth["build"]=Ota::build();auth["logv"]=2;
        serializeJson(auth,client);client.println();lastMessage=millis();break;
      }
    }
    if(client.connected()) {
      while(client.available()) {
        int value=client.read();if(value<0) break;char c=char(value);
        if(c=='\n') {
          rx.text[len]=0;JsonDocument doc;
          if(!deserializeJson(doc,rx.text) && doc["v"]==1) {
            if(doc["t"]=="authorized") {authorized=true;linkState=Authorized;Ota::confirmIfPending();}
            else if(authorized && doc["t"]=="ota_begin") {
              runOta(client,doc["size"] | 0u,doc["sha256"] | "",doc["version"] | "");
              lastMessage=millis();len=0;break;
            }
            else if(authorized && (doc["t"]=="state" || doc["t"]=="tail")) {
              // Drop rather than block the Wi-Fi task if UI is temporarily busy.
              xQueueSend(incoming,&rx,0);lastMessage=millis();
            }
          }
          len=0;
        } else if(len<4096) rx.text[len++]=c;
        else {client.stop();len=0;break;}
      }
      while(xQueueReceive(outgoing,&tx,0)==pdTRUE) if(authorized){client.print(tx.text);client.println();}
      if(millis()-lastMessage>15000) {client.stop();authorized=false;}
    }
    vTaskDelay(pdMS_TO_TICKS(20));
  }
}
static void begin() {
  mutex=xSemaphoreCreateMutex();incoming=xQueueCreate(3,sizeof(Frame));outgoing=xQueueCreate(2,sizeof(Frame));
  if(!mutex || !incoming || !outgoing) return;
  Preferences prefs;
  if(prefs.begin("agentlight",true)) {
    String blob=prefs.getString("wifi","");prefs.end();JsonDocument doc;
    if(blob.length() && !deserializeJson(doc,blob)) {
      settings.ssid=doc["ssid"] | "";settings.password=doc["password"] | "";settings.helper=doc["helper"] | "";
      settings.fingerprint=doc["fingerprint"] | "";settings.token=doc["token"] | "";
    }
  }
  // Keep TLS/discovery work off the Arduino UI core, where the panel ISR is
  // installed during board.begin().
  xTaskCreatePinnedToCore(task,"agentlight-wifi",20480,nullptr,1,nullptr,0);
}
}
