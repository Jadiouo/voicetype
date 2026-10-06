#include "capslock.h"
#include <cstdio>
using namespace voicetype;
static int failures=0;
#define CHECK(x) do{if(!(x)){std::fprintf(stderr,"FAIL line %d: %s\n",__LINE__,#x);++failures;}}while(0)
int main(){
    using B=CapsLockGesture::Begin;
    for(bool locked:{false,true}){
        CapsLockGesture g;
        CHECK(g.begin(":1",66,locked,100,100000)==B::Started);
        CHECK(g.begin(":1",66,!locked,101,110000)==B::Repeat);
        CHECK(!g.isLaterPress(":1",66,99));
        CHECK(!g.isLaterPress(":1",66,100));
        CHECK(g.isLaterPress(":1",66,101));
        CHECK(!g.isLaterPress(":2",66,101));
        CHECK(!g.isLaterPress(":1",67,101));
        CHECK(!g.isLaterPress(":1",66,2100));
        CHECK(g.saved().locked==locked); // repeat cannot overwrite original Lock
        CHECK(!g.release(":2",66,102,120000));
        CHECK(!g.release(":1",67,102,120000));
        CHECK(!g.release(":1",66,102,120000,false)); // programmatic notification
        CHECK(!g.release(":1",66,99,120000)); // older timestamp
        auto value=g.release(":1",66,102,120000);
        CHECK(value && value->locked==locked && value->keycode==66);
        CHECK(!g.release(":1",66,103,130000)); // one restoration only
    }
    CapsLockGesture g;
    CHECK(g.begin("",66,false,1,1)==B::Rejected);
    CHECK(g.begin(":1",0,false,1,1)==B::Rejected);
    CHECK(g.begin(":1",66,false,0,1)==B::Rejected);
    CHECK(g.begin(":1",66,true,100,100000)==B::Started);
    CHECK(g.begin(":2",66,false,100,100000)==B::Rejected);
    g.expire(2100000); CHECK(!g.active());
    CHECK(!g.release(":1",66,2101,2101000)); // expired IC/physical release: no late write
    CHECK(g.begin(":1",66,false,3000,3000000)==B::Started);
    CHECK(g.release(":1",66,3001,3001000).has_value());
    CHECK(g.begin(":1",66,true,4000,4000000)==B::Started);
    g.expire(5000000); CHECK(g.active()); // old gesture's deadline cannot clear new one
    CHECK(g.release(":1",66,4001,5000001).has_value());
    CHECK(g.begin(":1",66,true,0xfffffff0,6000000)==B::Started);
    CHECK(g.isLaterPress(":1",66,0x10));
    CHECK(!g.isLaterPress(":1",66,0xffffffe0));
    CHECK(g.release(":1",66,0x10,6032000).has_value()); // X timestamp wrap
    CHECK(g.begin(":1",66,true,7000,7000000)==B::Started);
    g.clear(); CHECK(!g.release(":1",66,7001,7001000)); // teardown/connection invalidation
    return failures?1:0;
}
