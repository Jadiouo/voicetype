// Actual XTest -> XKB -> VoiceType/XCB dispatch, exclusively on launcher-owned Xvfb.
#include "voicetype.h"
#include <fcitx/focusgroup.h>
#include <fcitx/addonmanager.h>
#include <fcitx-utils/event.h>
#include <X11/Xlib.h>
#include <X11/XKBlib.h>
#include <X11/keysym.h>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <sys/socket.h>
#include <unistd.h>
#include <set>
#include <vector>
extern "C" int XTestFakeKeyEvent(Display *, unsigned int, Bool, unsigned long);
class Client : public fcitx::InputContext {
public:
 explicit Client(fcitx::Instance &i):InputContext(i.inputContextManager(),"caps-isolated"){}
 ~Client() override {destroy();}
 const char *frontend() const override{return "isolated-x11-test";}
protected:
 void commitStringImpl(const std::string &) override{}
 void deleteSurroundingTextImpl(int,unsigned int) override{}
 void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override{}
 void updatePreeditImpl() override{}
};
int main(int argc, char **argv) {
 if(!std::getenv("VOICETYPE_ISOLATED_DISPLAY"))return 90;
 const bool remapped=argc>1&&std::string(argv[1])=="remapped";
 Display *d=XOpenDisplay(nullptr);if(!d)return 91;
 auto w=XCreateSimpleWindow(d,DefaultRootWindow(d),0,0,200,100,0,0,0);
 XSelectInput(d,w,KeyPressMask|KeyReleaseMask);XMapWindow(d,w);XSetInputFocus(d,w,RevertToParent,CurrentTime);XSync(d,False);
 char name[]="caps-test",disabled[]="--disable=all",enabled[]="--enable=xcb";
 char *instanceArgs[]={name,disabled,enabled,nullptr};
 fcitx::Instance instance(3,instanceArgs);instance.addonManager().registerDefaultLoader(nullptr);instance.initialize();
 fcitx::FocusGroup group(std::string("x11:")+std::getenv("DISPLAY"),instance.inputContextManager());
 std::unique_ptr<Client> client;
 auto create=[&](){client=std::make_unique<Client>(instance);client->setFocusGroup(&group);client->focusIn();};
 create();
 unsigned notices=0,failures=0,repeatNotices=0;bool dropRelease=false;int forceCode=-1;
 uint32_t lastCapsTime=0;bool lastCapsLocked=false;
 fcitx::FocusGroup unknownGroup("wayland:isolated",instance.inputContextManager());
 fcitx::FocusGroup otherDisplay("x11::65432",instance.inputContextManager());
 auto socketFds=[](){std::set<int> found;for(int fd=3;fd<1024;++fd){sockaddr_storage address{};socklen_t size=sizeof(address);if(getpeername(fd,reinterpret_cast<sockaddr*>(&address),&size)==0)found.insert(fd);}return found;};
 const auto beforeSockets=socketFds();
 voicetype::VoiceTypeConfig config;
 if(remapped&&!config.learnKey.setValue(fcitx::Key("Control+Shift+Caps_Lock")))return 94;
 voicetype::VoiceType addon(&instance,[&](const std::string &,const std::string &){++notices;},{},config);
 const auto afterSockets=socketFds();
 int observerFd=-1;
 for(int fd:afterSockets)if(!beforeSockets.count(fd)){if(observerFd!=-1)return 92;observerFd=fd;}
 if(observerFd<0)return 93;
 auto pump=[&](){
   while(XPending(d)){
    XEvent e;XNextEvent(d,&e);if(e.type!=KeyPress&&e.type!=KeyRelease)continue;
    if(!client||(dropRelease&&e.type==KeyRelease))continue;
    const auto sym=XLookupKeysym(&e.xkey,0);
    if(sym==XK_Caps_Lock&&e.type==KeyPress){lastCapsTime=e.xkey.time;lastCapsLocked=(e.xkey.state&LockMask)!=0;}
    fcitx::KeyEvent event(client.get(),fcitx::Key(static_cast<fcitx::KeySym>(sym),fcitx::KeyStates(e.xkey.state),forceCode<0?e.xkey.keycode:forceCode),e.type==KeyRelease,e.xkey.time);
    instance.postEvent(event);
   }
 };
 auto mods=[&](){XkbStateRec s{};XkbGetState(d,XkbUseCoreKbd,&s);return s.locked_mods;};
 auto inject=[&](KeySym sym,bool down){XTestFakeKeyEvent(d,XKeysymToKeycode(d,sym),down,0);XSync(d,False);pump();};
 auto pressLearnModifiers=[&](){inject(XK_Control_L,true);if(remapped)inject(XK_Shift_L,true);};
 auto releaseLearnModifiers=[&](){inject(XK_Control_L,false);if(remapped)inject(XK_Shift_L,false);};
 auto check=[&](const char *label,bool ok){std::printf("{\"case\":\"%s\",\"pass\":%s}\n",label,ok?"true":"false");if(!ok)++failures;};
 std::vector<std::function<void()>> steps;
 auto step=[&](std::function<void()> f){steps.push_back(std::move(f));};
 // Deliberately queue physical events before frontend dispatch, as a busy
 // application can: the observer fd is not drained during this callback.
 bool expectedLast=false;
 auto batchKey=[&](KeySym sym,bool down){XTestFakeKeyEvent(d,XKeysymToKeycode(d,sym),down,3);};
 for(bool ordinaryBetween:{false,true})for(bool initial:{false,true}){
   step([&,initial](){create();dropRelease=false;XkbLockModifiers(d,XkbUseCoreKbd,LockMask|Mod2Mask,(initial?LockMask:0)|Mod2Mask);XSync(d,False);pump();pressLearnModifiers();inject(XK_Caps_Lock,true);});
   step([&,ordinaryBetween](){
     batchKey(XK_Caps_Lock,false);
     if(ordinaryBetween){batchKey(XK_Control_L,false);if(remapped)batchKey(XK_Shift_L,false);batchKey(XK_Caps_Lock,true);batchKey(XK_Caps_Lock,false);batchKey(XK_Control_L,true);if(remapped)batchKey(XK_Shift_L,true);}
     batchKey(XK_Caps_Lock,true);XSync(d,False);pump();
     expectedLast=lastCapsLocked;
     client.reset();
   });
   step([&](){inject(XK_Caps_Lock,false);releaseLearnModifiers();});
   step([&,ordinaryBetween](){check(ordinaryBetween?"queued_ordinary_then_shortcut_keeps_last_raw_lock":"queued_two_shortcuts_keep_last_raw_lock",mods()==((expectedLast?LockMask:0)|Mod2Mask));});
 }
 // Each operation is separated by a real event-loop tick, processing native
 // XCB StateNotify before the next assertion. No restoration in the fixture.
 for(bool initial:{false,true})for(bool ctrlFirst:{false,true})for(int path:{0,1,2}){
   step([&,initial](){create();dropRelease=false;XkbLockModifiers(d,XkbUseCoreKbd,LockMask|Mod2Mask,(initial?LockMask:0)|Mod2Mask);XSync(d,False);pump();});
   step([&](){pressLearnModifiers();});
   step([&](){inject(XK_Caps_Lock,true);});
   step([&,initial,path](){check("press_preserves_lock_and_numlock",mods()==((initial?LockMask:0)|Mod2Mask));if(path==1)dropRelease=true;if(path==2)client.reset();});
   step([&,ctrlFirst](){inject(ctrlFirst?XK_Control_L:XK_Caps_Lock,false);});
   step([&,ctrlFirst](){inject(ctrlFirst?XK_Caps_Lock:XK_Control_L,false);if(remapped)inject(XK_Shift_L,false);});
   step([&,initial,path](){check(path==0?"normal_release":path==1?"xkb_only_release":"ic_teardown_release",mods()==((initial?LockMask:0)|Mod2Mask));});
 }
 step([&](){create();dropRelease=false;XkbLockModifiers(d,XkbUseCoreKbd,LockMask,0);XSync(d,False);});
 step([&](){inject(XK_Caps_Lock,true);});
 step([&](){inject(XK_Caps_Lock,false);});
 step([&](){check("ordinary_caps_still_toggles",(mods()&LockMask)!=0);});
 step([&](){XkbLockModifiers(d,XkbUseCoreKbd,LockMask,0);XSync(d,False);forceCode=0;});
 step([&](){pressLearnModifiers();});step([&](){inject(XK_Caps_Lock,true);});
 step([&](){inject(XK_Caps_Lock,false);releaseLearnModifiers();});
 step([&](){check("missing_physical_code_abstains",(mods()&LockMask)!=0);forceCode=-1;});
 // No event-loop gap between select/restore, teardown, and physical release.
 for(bool initial:{false,true}){
   step([&,initial](){create();XkbLockModifiers(d,XkbUseCoreKbd,LockMask,initial?LockMask:0);XSync(d,False);pressLearnModifiers();});
   step([&](){inject(XK_Caps_Lock,true);client.reset();inject(XK_Caps_Lock,false);releaseLearnModifiers();});
   step([&,initial](){check("immediate_release_after_teardown",bool(mods()&LockMask)==initial);});
 }
 for(auto *other:{&unknownGroup,&otherDisplay}){
   step([&,other](){create();client->setFocusGroup(other);client->focusIn();XkbLockModifiers(d,XkbUseCoreKbd,LockMask,0);XSync(d,False);});
   step([&](){pressLearnModifiers();});step([&](){inject(XK_Caps_Lock,true);});
   step([&](){inject(XK_Caps_Lock,false);releaseLearnModifiers();});
   step([&](){check("unverified_display_abstains",(mods()&LockMask)!=0);});
 }
 step([&](){create();XkbLockModifiers(d,XkbUseCoreKbd,LockMask,LockMask);XSync(d,False);pressLearnModifiers();});
 step([&](){inject(XK_Caps_Lock,true);repeatNotices=notices;});
 step([&](){
   auto repeatStates=fcitx::KeyStates(fcitx::KeyState::Ctrl)|fcitx::KeyState::Repeat;
   if(remapped)repeatStates|=fcitx::KeyState::Shift;
   fcitx::KeyEvent repeat(client.get(),fcitx::Key(FcitxKey_Caps_Lock,repeatStates,66),false,lastCapsTime+1);
   instance.postEvent(repeat);
 });
 step([&](){releaseLearnModifiers();});step([&](){inject(XK_Caps_Lock,false);});
 step([&](){check("repeat_keeps_original_and_learns_once",notices==repeatNotices&&(mods()&LockMask)!=0);});
 step([&](){XkbLockModifiers(d,XkbUseCoreKbd,LockMask,LockMask);XSync(d,False);});
 step([&](){pressLearnModifiers();});step([&](){inject(XK_Caps_Lock,true);});
 step([&](){client.reset();});
 for(int i=0;i<110;++i)step([](){}); // >2s: later release must not restore.
 step([&](){releaseLearnModifiers();});step([&](){inject(XK_Caps_Lock,false);});
 step([&](){check("expired_release_never_writes_saved_lock",(mods()&LockMask)==0);});
 step([&](){create();XkbLockModifiers(d,XkbUseCoreKbd,LockMask,0);XSync(d,False);pressLearnModifiers();});
 step([&](){inject(XK_Caps_Lock,true);shutdown(observerFd,SHUT_RDWR);});
 step([&](){inject(XK_Caps_Lock,false);releaseLearnModifiers();});
 for(int i=0;i<5;++i)step([](){});
 step([&](){XkbLockModifiers(d,XkbUseCoreKbd,LockMask,0);XSync(d,False);pressLearnModifiers();});
 step([&](){inject(XK_Caps_Lock,true);});
 step([&](){inject(XK_Caps_Lock,false);releaseLearnModifiers();});
 step([&](){check("observer_disconnect_keeps_loop_alive_and_abstains",(mods()&LockMask)!=0);});
 size_t index=0;
 auto timer=instance.eventLoop().addTimeEvent(CLOCK_MONOTONIC,fcitx::now(CLOCK_MONOTONIC)+20000,1,
   [&](fcitx::EventSourceTime *t,uint64_t){pump();if(index==steps.size()){instance.eventLoop().exit();return false;}steps[index++]();t->setNextInterval(20000);t->setOneShot();return true;});
 instance.eventLoop().exec();
 std::printf("{\"notices\":%u,\"failures\":%u}\n",notices,failures);
 client.reset();XDestroyWindow(d,w);XCloseDisplay(d);
 return failures?1:0;
}
