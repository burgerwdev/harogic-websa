#include "iqs_usb_pipeline.hpp"
#include <cassert>
#include <cstdlib>
#include <cstdio>
#include <deque>
#include <set>
#include <unordered_map>
#include <string>
#include <unistd.h>
using namespace websa_iqs_usb;
struct Fake {
    std::deque<libusb_transfer*> pending;
    std::unordered_map<libusb_transfer*,uint64_t> sequences;
    std::set<libusb_transfer*> cancelled;
    uint64_t next=0;
    int allocated=0,released=0,submissions=0,callbacks=0,bulk_calls=0,max_pending=0;
    int allocation_fail=-1,submission_fail=-1,event_error=0;
    int bad_sequence=-1,partial_sequence=-1;
    int bad_length=0,callback_delay_ms=0;
    libusb_transfer_status bad_status=LIBUSB_TRANSFER_TIMED_OUT;
} fake;
static int LIBUSB_CALL init(libusb_context**) {return 0;}
static libusb_transfer* LIBUSB_CALL allocate(int) {
    if (fake.allocation_fail==fake.allocated) return nullptr;
    ++fake.allocated;return static_cast<libusb_transfer*>(std::calloc(1,sizeof(libusb_transfer)));
}
static void LIBUSB_CALL release(libusb_transfer* transfer) {
    assert(fake.sequences.count(transfer)==0);++fake.released;std::free(transfer);
}
static int LIBUSB_CALL submit(libusb_transfer* transfer) {
    if (fake.submission_fail==fake.submissions++) return LIBUSB_ERROR_NO_MEM;
    assert(!fake.sequences.count(transfer));
    fake.sequences[transfer]=fake.next++;fake.pending.push_back(transfer);
    fake.max_pending=std::max(fake.max_pending,static_cast<int>(fake.pending.size()));return 0;
}
static int LIBUSB_CALL cancel(libusb_transfer* transfer) {
    assert(fake.sequences.count(transfer));fake.cancelled.insert(transfer);return 0;
}
static int LIBUSB_CALL events(libusb_context*,timeval*,int*) {
    if (fake.event_error) {const int result=fake.event_error;fake.event_error=0;return result;}
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    // Complete a whole USB batch in reverse callback order. Delivery must still
    // follow submission order, including batches completed before their read.
    while (!fake.pending.empty()) {
        auto* transfer=fake.pending.back();fake.pending.pop_back();
        const auto sequence=fake.sequences.at(transfer);fake.sequences.erase(transfer);
        transfer->status=LIBUSB_TRANSFER_COMPLETED;transfer->actual_length=transfer->length;
        if (fake.cancelled.erase(transfer)) {transfer->status=LIBUSB_TRANSFER_CANCELLED;transfer->actual_length=0;}
        else {
            if (static_cast<int64_t>(sequence)==fake.bad_sequence) {
                transfer->status=fake.bad_status;transfer->actual_length=fake.bad_length;
            }else if (static_cast<int64_t>(sequence)==fake.partial_sequence) transfer->actual_length=127;
            if (transfer->actual_length) {
                assert(transfer->actual_length>=8 && transfer->actual_length<=transfer->length);
                std::memcpy(transfer->buffer,&sequence,sizeof(sequence));
                std::memset(transfer->buffer+8,(sequence*7)&255,transfer->actual_length-8);
            }
        }
        ++fake.callbacks;transfer->callback(transfer);
    }
    // A completed callback may precede the pump's return from event handling.
    if (fake.callback_delay_ms) std::this_thread::sleep_for(std::chrono::milliseconds(fake.callback_delay_ms));
    return 0;
}
static int LIBUSB_CALL bulk(libusb_device_handle*,unsigned char,unsigned char* data,int length,int* actual,unsigned) {
    ++fake.bulk_calls;if (data && length) data[0]=91;if (actual) *actual=length;return 0;
}
static Api api() {return {init,bulk,allocate,release,submit,cancel,events};}
static auto* device=reinterpret_cast<libusb_device_handle*>(1);
static std::array<unsigned char,65536> bytes;
static int read(Pipeline& pipeline,int* actual,unsigned endpoint=0x81,int length=65536) {
    return pipeline.read(device,endpoint,bytes.data(),length,actual,250);
}
static uint64_t sequence() {uint64_t result;std::memcpy(&result,bytes.data(),8);return result;}
static void reset() {assert(fake.pending.empty() && fake.sequences.empty());fake=Fake{};}
static void released() {assert(fake.pending.empty() && fake.sequences.empty());assert(fake.allocated==fake.released);}
struct CaptureStderr {
    FILE* file=std::tmpfile();
    int saved=dup(STDERR_FILENO);
    CaptureStderr() {assert(file && saved>=0);std::fflush(stderr);assert(dup2(fileno(file),STDERR_FILENO)>=0);}
    std::string finish() {
        std::fflush(stderr);assert(dup2(saved,STDERR_FILENO)>=0);close(saved);saved=-1;
        std::rewind(file);std::string output;std::array<char,4096> buffer{};
        while (const auto count=std::fread(buffer.data(),1,buffer.size(),file)) output.append(buffer.data(),count);
        std::fclose(file);file=nullptr;return output;
    }
    ~CaptureStderr() {if (saved>=0) {dup2(saved,STDERR_FILENO);close(saved);}if (file) std::fclose(file);}
};
static uint64_t diagnostic_value(const std::string& log,const char* key) {
    const std::string marker=std::string(key)+"=";const auto position=log.find(marker);
    assert(position!=std::string::npos);char* end=nullptr;
    const auto value=std::strtoull(log.c_str()+position+marker.size(),&end,10);
    assert(end!=log.c_str()+position+marker.size());return value;
}
int main() {
    {
        reset();Pipeline pipeline(api());int actual=0;
        assert(!pipeline.start(true));assert(read(pipeline,&actual)==0 && fake.bulk_calls==1 && !fake.allocated);
        pipeline.context(nullptr);assert(!pipeline.start(false));assert(read(pipeline,&actual)==0 && fake.bulk_calls==2);
        assert(pipeline.start(true));
        assert(read(pipeline,&actual,1,32)==0 && fake.bulk_calls==3 && !fake.allocated);
        fake.event_error=LIBUSB_ERROR_INTERRUPTED;
        for (unsigned i=0;i<389;++i) {
            assert(read(pipeline,&actual)==0 && actual==65536 && sequence()==i);
            for (int n=8;n<actual;++n) assert(bytes[n]==((i*7)&255));
        }
        pipeline.stop();released();assert(fake.cancelled.empty());
        assert(fake.max_pending==4 && fake.allocated==4);
        assert(pipeline.stats().delivered==389 && pipeline.stats().failed==0);
        // A new stream cannot receive the cancelled tail of the previous one.
        fake.next=1000;assert(pipeline.start(true));assert(read(pipeline,&actual)==0 && sequence()==1000);
        pipeline.stop();released();
    }
    {
        reset();fake.partial_sequence=0;Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=0;
        assert(read(pipeline,&actual)==0 && actual==127 && sequence()==0);
        assert(read(pipeline,&actual)==0 && actual==65536 && sequence()==1);
        pipeline.stop();released();
    }
    {
        // Preserve the successful prefix and the original partial USB error,
        // report its bounded timing tail, then retire every buffer before reopen.
        reset();fake.bad_sequence=6;fake.bad_length=49152;fake.callback_delay_ms=3;
        Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=0;
        CaptureStderr capture;
        for (unsigned i=0;i<6;++i) assert(read(pipeline,&actual)==0 && actual==65536 && sequence()==i);
        assert(read(pipeline,&actual)==LIBUSB_ERROR_TIMEOUT && actual==49152 && sequence()==6);
        for (int n=8;n<actual;++n) assert(bytes[n]==42);
        released();assert(!pipeline.enabled() && !pipeline.handoff_overflow());
        assert(pipeline.stats().delivered==6 && pipeline.stats().failed==1);
        const auto log=capture.finish();std::fputs(log.c_str(),stderr);
        assert(log.find("status=2 bytes=49152 timeout=250")!=std::string::npos);
        assert(log.find("SDK IQS USB tail:")!=std::string::npos);
        assert(log.find("callback_clock=userspace_monotonic")!=std::string::npos);
        assert(diagnostic_value(log,"pump_gap_max_ns")>=4000000);
        assert(diagnostic_value(log,"events_max_ns")>=4000000);
        assert(diagnostic_value(log,"submit_callback_max_ns")>=1000000);
        assert(diagnostic_value(log,"callback_resubmit_max_ns")>=3000000);
        assert(diagnostic_value(log,"awaiting_callback")<=Pipeline::transfer_count());
        assert(diagnostic_value(log,"completed_unhandled")<=Pipeline::transfer_count());
        unsigned tails=0;std::string::size_type at=0;
        while ((at=log.find("SDK IQS USB tail:",at))!=std::string::npos) {++tails;++at;}
        assert(tails>=1 && tails<=64);
        fake.next=1000;fake.bad_sequence=-1;fake.callback_delay_ms=0;
        assert(pipeline.start(true));assert(read(pipeline,&actual)==0 && actual==65536 && sequence()==1000);
        pipeline.stop();released();assert(fake.cancelled.empty());
    }
    for (auto [status,expected]:{std::pair{LIBUSB_TRANSFER_TIMED_OUT,LIBUSB_ERROR_TIMEOUT},
            {LIBUSB_TRANSFER_STALL,LIBUSB_ERROR_PIPE},{LIBUSB_TRANSFER_NO_DEVICE,LIBUSB_ERROR_NO_DEVICE},
            {LIBUSB_TRANSFER_OVERFLOW,LIBUSB_ERROR_OVERFLOW},{LIBUSB_TRANSFER_ERROR,LIBUSB_ERROR_IO}}) {
        reset();fake.bad_sequence=0;fake.bad_status=status;Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=99;
        assert(read(pipeline,&actual)==expected && actual==0);released();
        assert(!pipeline.handoff_overflow());
        assert(!pipeline.enabled() && pipeline.stats().delivered==0 && pipeline.stats().failed==1);
        // Do not busy-loop over a cached failed transfer on the next vendor call.
        assert(read(pipeline,&actual)==0 && fake.bulk_calls==1);
    }
    {
        reset();fake.submission_fail=4;Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=0;
        assert(read(pipeline,&actual)==0 && actual==65536 && sequence()==0);
        assert(read(pipeline,&actual)==LIBUSB_ERROR_NO_MEM && actual==0);released();
        assert(pipeline.stats().delivered==1);
    }
    for (int failure=0;failure<4;++failure) {
        reset();fake.submission_fail=failure;Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=99;
        assert(read(pipeline,&actual)==LIBUSB_ERROR_NO_MEM && actual==0);released();
        reset();fake.allocation_fail=failure;Pipeline other(api());other.context(nullptr);assert(other.start(true));
        assert(read(other,&actual)==LIBUSB_ERROR_NO_MEM && actual==0);released();
    }
    {
        reset();Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=0;
        assert(read(pipeline,&actual)==0);const auto bulk_calls=fake.bulk_calls;
        assert(read(pipeline,&actual,0x81,1024)==LIBUSB_ERROR_INVALID_PARAM && actual==0);
        assert(fake.bulk_calls==bulk_calls);released();
    }
    {
        reset();Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=99;fake.event_error=LIBUSB_ERROR_IO;
        assert(read(pipeline,&actual)==LIBUSB_ERROR_IO && actual==0);released();
    }
    {
        // The SDK owner can pause while USB keeps making progress; byte order
        // remains exact when the owner resumes and drains the buffered prefix.
        reset();Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=0;
        assert(read(pipeline,&actual)==0 && sequence()==0);
        auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(2);
        while (pipeline.stats().submitted<100 && std::chrono::steady_clock::now()<deadline)
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        assert(pipeline.stats().submitted>=100 && pipeline.stats().peak_queued_packets>4);
        for (unsigned i=1;i<200;++i) assert(read(pipeline,&actual)==0 && sequence()==i);
        pipeline.stop();released();assert(pipeline.stats().failed==0);
    }
    {
        // A stalled owner cannot create an unbounded queue or silently overwrite.
        reset();Pipeline pipeline(api());pipeline.context(nullptr);assert(pipeline.start(true));int actual=0;
        assert(read(pipeline,&actual)==0 && sequence()==0);
        auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(2);
        while (!pipeline.stats().failed && std::chrono::steady_clock::now()<deadline)
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        assert(Pipeline::handoff_capacity()==1024);
        assert(pipeline.handoff_overflow());
        assert(pipeline.stats().failed==1 && pipeline.stats().peak_queued_packets==1024);
        for (unsigned i=1;i<=1024;++i) assert(read(pipeline,&actual)==0 && sequence()==i);
        assert(read(pipeline,&actual)==LIBUSB_ERROR_OVERFLOW && actual==0);released();
        assert(pipeline.handoff_overflow());
        assert(pipeline.start(true));assert(!pipeline.handoff_overflow());pipeline.stop();
    }
    std::puts("PASS: bounded USB ordering, byte integrity, cancellation, source restart, fallback, partial reads and failures");
}
