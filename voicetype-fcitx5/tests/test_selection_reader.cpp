// Actual subprocess + Fcitx event loop: bounded output, fast exit, cancellation,
// timeouts kill/reap the owned process group without blocking UI heartbeats.
#include "selection.h"
#include <fcitx-utils/event.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include <filesystem>
#include <fstream>
#include <iostream>

using namespace voicetype;
int main() {
    int failures = 0;
    auto check = [&](bool ok) { if (!ok) { ++failures; std::cerr << "selection reader assertion failed\n"; } };
    char temp[] = "/tmp/voicetype-reader-XXXXXX";
    if (!mkdtemp(temp)) { return 1; }
    const std::string dir = temp, helper = dir + "/helper", pidfile = dir + "/pids";
    std::ofstream script(helper);
    script << "#!/usr/bin/python3\nimport os,sys,time,json\nmode=sys.argv[-1]\n"
              "if mode=='large': print('x'*9000); sys.exit()\n"
              "if mode=='bad': print('not json'); sys.exit()\n"
              "if mode in ('timeout','cancel'):\n"
              " child=os.fork()\n"
              " if child: open('" << pidfile << "','w').write(str(os.getpid())+' '+str(child))\n"
              " time.sleep(20)\n"
              "print(json.dumps({'type':'selection','program':'fixture','text':'GitHub','context_id':':1.2:/field','value':'0x1:42'}))\n";
    script.close(); chmod(helper.c_str(),0700);
    prctl(PR_SET_CHILD_SUBREAPER, 1);
    for (const std::string mode : {"fast", "large", "bad", "timeout", "cancel"}) {
        std::cerr << "reader case " << mode << "\n";
        const int rounds = mode == "fast" ? 40 : 1;
        for (int i=0;i<rounds;++i) {
            fcitx::EventLoop loop;
            SelectionReader reader(&loop);
            bool called=false; int callbacks=0, heartbeats=0;
            const auto began=fcitx::now(CLOCK_MONOTONIC);
            check(reader.start(helper,mode,[&](const IpcMessage &m) {
                called=true; ++callbacks;
                if (mode == "fast") { check(m.type=="selection" && m.text=="GitHub"); }
                else if(mode == "large") { check(m.code=="output_too_large"); }
                else if(mode == "timeout") { check(m.code=="timeout"); }
                else if(mode == "bad") { check(m.code=="helper_failed"); }
            }));
            auto tick=loop.addTimeEvent(CLOCK_MONOTONIC,began+1000,1,
                [&](fcitx::EventSourceTime *event,uint64_t) {
                    ++heartbeats;
                    const auto now=fcitx::now(CLOCK_MONOTONIC);
                    if(mode=="cancel" && now-began>100000 && reader.busy()) { reader.cancel(); }
                    if(!reader.busy()) { loop.exit(); return false; }
                    if(now-began>1500000) { check(false); loop.exit(); return false; }
                    event->setTime(now+1000);event->setOneShot();return true;
                });
            loop.exec();
            check(callbacks==(mode=="cancel" ? 0:1));
            check(called==(mode!="cancel"));
            if(mode=="timeout") { check(heartbeats>100 && fcitx::now(CLOCK_MONOTONIC)-began<1000000); }
            if(mode=="timeout" || mode=="cancel") {
                std::ifstream input(pidfile);int child=-1,grandchild=-1;input>>child>>grandchild;
                check(child>0 && grandchild>0);
                int status=0;
                check(waitpid(child,&status,WNOHANG)==-1 && errno==ECHILD);
                // We are subreaper solely in this test; the reader owns its
                // direct child while SIGKILL reaches the descendant as well.
                check(waitpid(grandchild,&status,0)==grandchild);
                check(WIFSIGNALED(status) && WTERMSIG(status)==SIGKILL);
            }
        }
    }
    std::filesystem::remove_all(dir);
    return failures ? 1 : 0;
}
