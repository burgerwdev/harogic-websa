#pragma once
// A bounded USB pump keeps reads posted while the SDK owner handles FFT/control
// or downstream handoff. Only raw bytes cross threads; SDK calls keep their owner.
#include <libusb-1.0/libusb.h>
#include <dlfcn.h>
#include <array>
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <thread>
#include <atomic>
#include <condition_variable>
#include <memory>
#include <mutex>
#include <new>
#if defined(__linux__)
#include <pthread.h>
#endif

namespace websa_iqs_usb {
// Diagnostic owner phase, sampled by the pump into its fixed failure tail.
// No logging, allocation, or extra lock in the SDK call path.
enum class OwnerPhase : unsigned { idle, operation, fetch, copy, fft };
struct Api {
    decltype(&libusb_init) init;
    decltype(&libusb_bulk_transfer) bulk;
    decltype(&libusb_alloc_transfer) allocate;
    decltype(&libusb_free_transfer) release;
    decltype(&libusb_submit_transfer) submit;
    decltype(&libusb_cancel_transfer) cancel;
    decltype(&libusb_handle_events_timeout_completed) events;
    bool valid() const { return init && bulk && allocate && release && submit && cancel && events; }
};
template<class T> inline T next_symbol(const char* name) {
    return reinterpret_cast<T>(dlsym(RTLD_NEXT,name));
}
inline Api runtime_api() {
    return {next_symbol<decltype(Api::init)>("libusb_init"),
        next_symbol<decltype(Api::bulk)>("libusb_bulk_transfer"),
        next_symbol<decltype(Api::allocate)>("libusb_alloc_transfer"),
        next_symbol<decltype(Api::release)>("libusb_free_transfer"),
        next_symbol<decltype(Api::submit)>("libusb_submit_transfer"),
        next_symbol<decltype(Api::cancel)>("libusb_cancel_transfer"),
        next_symbol<decltype(Api::events)>("libusb_handle_events_timeout_completed")};
}
struct Stats {
    uint64_t submitted=0,delivered=0,failed=0,cancelled=0,discarded=0;
    uint64_t peak_queued_packets=0,max_queue_age_ns=0;
};
class Pipeline {
    static constexpr unsigned depth=4;
    // About 266 ms at full rate, covering the measured 233 ms scheduling tail.
    // overflow is terminal, never silent overwrite or an expanding backlog.
    static constexpr unsigned capacity=1024;
    static constexpr int packet_bytes=65536;
    using Clock=std::chrono::steady_clock;
    struct Job {
        libusb_transfer* transfer=nullptr;
        std::array<unsigned char,packet_bytes> bytes;
        std::atomic<bool> done{true};
        int event_done=1; // libusb reads this only while holding its event lock
        bool submitted=false; // only the USB pump reads/writes this field
    };
    struct Packet {
        std::array<unsigned char,packet_bytes> bytes;
        int length=0,result=0;
        Clock::time_point queued;
    };
    struct Ring {
        std::array<Job,depth> jobs;
        std::unique_ptr<Packet[]> packets;
        libusb_device_handle* device=nullptr;
        unsigned timeout=0,head=0,tail=0,count=0;
    };
    Api api_;
    libusb_context* context_=nullptr;
    bool context_known_=false,enabled_=false;
    std::unique_ptr<Ring> ring_;
    std::thread pump_;
    std::atomic<bool> stopping_{false};
    mutable std::mutex mutex_;
    std::condition_variable available_;
    int terminal_error_=0;
    std::atomic<bool> handoff_overflow_{false};
    Stats stats_;
    Clock::time_point next_report_{};
    Clock::time_point last_read_{};
    uint64_t read_gap_max_ns_=0;
    std::atomic<OwnerPhase> owner_phase_{OwnerPhase::idle};
    struct Observation {int64_t at_ns=0;uint64_t submitted=0,delivered=0;unsigned queued=0;OwnerPhase phase=OwnerPhase::idle;};
    std::array<Observation,64> observations_{};
    unsigned observation_next_=0;
    Clock::time_point next_observation_{};
    static void LIBUSB_CALL complete(libusb_transfer* transfer) {
        // libusb may dispatch this on another thread servicing the same context.
        // Publish fields written by libusb before the pump inspects the transfer.
        auto& job=*static_cast<Job*>(transfer->user_data);
        job.event_done=1;
        job.done.store(true,std::memory_order_release);
    }
    static int error(libusb_transfer_status status) {
        switch(status) {
        case LIBUSB_TRANSFER_COMPLETED:return 0;
        case LIBUSB_TRANSFER_TIMED_OUT:return LIBUSB_ERROR_TIMEOUT;
        case LIBUSB_TRANSFER_CANCELLED:return LIBUSB_ERROR_INTERRUPTED;
        case LIBUSB_TRANSFER_STALL:return LIBUSB_ERROR_PIPE;
        case LIBUSB_TRANSFER_NO_DEVICE:return LIBUSB_ERROR_NO_DEVICE;
        case LIBUSB_TRANSFER_OVERFLOW:return LIBUSB_ERROR_OVERFLOW;
        default:return LIBUSB_ERROR_IO;
        }
    }
    void fail(int result) {
        {std::lock_guard<std::mutex> lock(mutex_);
            if (!terminal_error_) {terminal_error_=result;++stats_.failed;}}
        stopping_.store(true,std::memory_order_release);available_.notify_all();
    }
    int submit(Job& job) {
        job.event_done=0;
        job.done.store(false,std::memory_order_relaxed);
        const int result=api_.submit(job.transfer);
        job.submitted=!result;
        if (result) {
            job.done.store(true,std::memory_order_release);
            std::fprintf(stderr,"SDK IQS USB submit failed: status=%d\n",result);
        }else {std::lock_guard<std::mutex> lock(mutex_);++stats_.submitted;}
        return result;
    }
    bool pending() const {
        for (const auto& job:ring_->jobs)
            if (job.submitted && !job.done.load(std::memory_order_acquire)) return true;
        return false;
    }
    void run(unsigned char endpoint) {
#if defined(__linux__)
        pthread_setname_np(pthread_self(),"iqs-usb-pump");
#endif
        int failure=0;
        for (auto& job:ring_->jobs) {
            job.transfer=api_.allocate(0);
            if (!job.transfer) {failure=LIBUSB_ERROR_NO_MEM;break;}
            libusb_fill_bulk_transfer(job.transfer,ring_->device,endpoint,job.bytes.data(),packet_bytes,
                complete,&job,ring_->timeout);
            failure=submit(job);if (failure) break;
        }
        if (failure) fail(failure);
        unsigned next=0;
        while (!stopping_.load(std::memory_order_acquire)) {
            auto& job=ring_->jobs[next];
            if (!job.done.load(std::memory_order_acquire)) {
                timeval wait{0,10000};
                const int result=api_.events(context_,&wait,&job.event_done);
                if (result && result!=LIBUSB_ERROR_INTERRUPTED) {
                    std::fprintf(stderr,"SDK IQS USB events failed: status=%d\n",result);
                    fail(result);
                }
                continue;
            }
            const int actual=job.transfer->actual_length;
            int result=error(job.transfer->status);
            if (actual<0 || actual>packet_bytes) result=LIBUSB_ERROR_OVERFLOW;
            bool overflow=false;
            uint64_t read_gap=0,max_read_gap=0,oldest_age=0,submitted=0,delivered=0;
            std::array<Observation,64> history{};
            unsigned history_next=0;
            {
                std::lock_guard<std::mutex> lock(mutex_);
                const auto now=Clock::now();
                if (now>=next_observation_) {
                    observations_[observation_next_]={std::chrono::duration_cast<std::chrono::nanoseconds>(now.time_since_epoch()).count(),
                        stats_.submitted,stats_.delivered,ring_->count,owner_phase_.load(std::memory_order_relaxed)};
                    observation_next_=(observation_next_+1)%observations_.size();
                    next_observation_=now+std::chrono::milliseconds(10);
                }
                overflow=ring_->count==capacity;
                if (overflow) {
                    read_gap=std::chrono::duration_cast<std::chrono::nanoseconds>(now-last_read_).count();
                    max_read_gap=read_gap_max_ns_;
                    oldest_age=std::chrono::duration_cast<std::chrono::nanoseconds>(now-ring_->packets[ring_->head].queued).count();
                    submitted=stats_.submitted;delivered=stats_.delivered;
                    history=observations_;history_next=observation_next_;
                }
                if (!overflow) {
                    auto& packet=ring_->packets[ring_->tail];
                    packet.length=actual>=0 && actual<=packet_bytes ? actual : 0;
                    packet.result=result;packet.queued=Clock::now();
                    if (packet.length) std::memcpy(packet.bytes.data(),job.bytes.data(),packet.length);
                    ring_->tail=(ring_->tail+1)%capacity;++ring_->count;
                    stats_.peak_queued_packets=std::max(stats_.peak_queued_packets,uint64_t(ring_->count));
                }
            }
            available_.notify_one();
            if (overflow) {
                std::fprintf(stderr,"SDK IQS USB handoff full: packets=%u read_gap_ns=%llu read_gap_max_ns=%llu oldest_age_ns=%llu submitted=%llu delivered=%llu\n",
                    capacity,static_cast<unsigned long long>(read_gap),static_cast<unsigned long long>(max_read_gap),
                    static_cast<unsigned long long>(oldest_age),static_cast<unsigned long long>(submitted),
                    static_cast<unsigned long long>(delivered));
                for (unsigned i=0;i<history.size();++i) {
                    const auto& row=history[(history_next+i)%history.size()];
                    if (row.at_ns) std::fprintf(stderr,"SDK IQS USB tail: monotonic_ns=%lld queued=%u submitted=%llu delivered=%llu owner_phase=%u\n",
                        static_cast<long long>(row.at_ns),row.queued,
                        static_cast<unsigned long long>(row.submitted),static_cast<unsigned long long>(row.delivered),
                        static_cast<unsigned>(row.phase));
                }
                handoff_overflow_.store(true,std::memory_order_release);
                fail(LIBUSB_ERROR_OVERFLOW);break;
            }
            job.submitted=false;
            if (result) {
                std::fprintf(stderr,"SDK IQS USB transfer failed: status=%d bytes=%d timeout=%u\n",
                    static_cast<int>(job.transfer->status),actual,ring_->timeout);
                fail(result);break;
            }
            failure=submit(job);
            if (failure) {fail(failure);break;}
            next=(next+1)%depth;
        }
        for (auto& job:ring_->jobs) {
            if (job.submitted && !job.done.load(std::memory_order_acquire)) {
                api_.cancel(job.transfer);
                std::lock_guard<std::mutex> lock(mutex_);++stats_.cancelled;
            }
        }
        // Do not release a transfer or buffer before its cancellation callback.
        // A stuck event loop keeps the SDK owner blocked in join; Rust's existing
        // owner timeout parks it rather than closing/reopening a live handle.
        while (pending()) {
            timeval wait{0,10000};const int result=api_.events(context_,&wait,nullptr);
            if (result && result!=LIBUSB_ERROR_INTERRUPTED)
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
        }
        for (auto& job:ring_->jobs) if (job.transfer) api_.release(job.transfer);
        available_.notify_all();
    }
public:
    void owner_phase(OwnerPhase phase) {owner_phase_.store(phase,std::memory_order_relaxed);}
    static constexpr unsigned transfer_count() {return depth;}
    static constexpr unsigned handoff_capacity() {return capacity;}
    explicit Pipeline(Api api):api_(api) {}
    ~Pipeline() {stop();}
    Pipeline(const Pipeline&)=delete;
    Pipeline& operator=(const Pipeline&)=delete;
    void context(libusb_context* value) {context_=value;context_known_=true;}
    const Api& api() const {return api_;}
    bool start(bool full_rate) {
        stop();handoff_overflow_.store(false,std::memory_order_release);enabled_=full_rate && context_known_ && api_.valid();
        next_report_=Clock::now()+std::chrono::seconds(5);return enabled_;
    }
    Stats stats() const {std::lock_guard<std::mutex> lock(mutex_);return stats_;}
    bool handoff_overflow() const {return handoff_overflow_.load(std::memory_order_acquire);}
    bool enabled() const {return enabled_;}
    void stop() {
        enabled_=false;stopping_.store(true,std::memory_order_release);
        available_.notify_all();
        if (pump_.joinable()) pump_.join();
        {std::lock_guard<std::mutex> lock(mutex_);
            if (ring_) stats_.discarded+=ring_->count;
            ring_.reset();terminal_error_=0;}
    }
    int read(libusb_device_handle* device,unsigned char endpoint,unsigned char* data,
            int length,int* transferred,unsigned timeout) {
        if (transferred) *transferred=0;
        if (!enabled_ || endpoint!=0x81)
            return api_.bulk ? api_.bulk(device,endpoint,data,length,transferred,timeout) : LIBUSB_ERROR_NOT_SUPPORTED;
        if (length!=packet_bytes || !data) {
            std::fprintf(stderr,"SDK IQS USB unexpected read: length=%d\n",length);
            fail(LIBUSB_ERROR_INVALID_PARAM);stop();return LIBUSB_ERROR_INVALID_PARAM;
        }
        if (!ring_) {
            auto ring=std::unique_ptr<Ring>(new(std::nothrow) Ring);
            if (ring) ring->packets.reset(new(std::nothrow) Packet[capacity]);
            if (!ring || !ring->packets) {fail(LIBUSB_ERROR_NO_MEM);stop();return LIBUSB_ERROR_NO_MEM;}
            ring->device=device;ring->timeout=timeout;ring_=std::move(ring);
            last_read_=Clock::now();read_gap_max_ns_=0;
            observations_={};observation_next_=0;next_observation_=last_read_;
            stopping_.store(false,std::memory_order_release);
            try {pump_=std::thread([this,endpoint]{run(endpoint);});}
            catch (...) {fail(LIBUSB_ERROR_NO_MEM);stop();return LIBUSB_ERROR_NO_MEM;}
        }
        if (ring_->device!=device || ring_->timeout!=timeout) {
            fail(LIBUSB_ERROR_INVALID_PARAM);stop();return LIBUSB_ERROR_INVALID_PARAM;
        }
        std::unique_lock<std::mutex> lock(mutex_);
        const auto entered=Clock::now();
        read_gap_max_ns_=std::max(read_gap_max_ns_,static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::nanoseconds>(entered-last_read_).count()));
        last_read_=entered;
        // libusb uses timeout==0 for an unlimited wait. Cancellation still wakes it.
        const auto ready=[&]{return ring_->count || terminal_error_;};
        if (timeout) {
            if (!available_.wait_for(lock,std::chrono::milliseconds(timeout),ready)) {
                lock.unlock();fail(LIBUSB_ERROR_TIMEOUT);stop();return LIBUSB_ERROR_TIMEOUT;
            }
        }else available_.wait(lock,ready);
        int result=terminal_error_;
        if (ring_->count) {
            auto& packet=ring_->packets[ring_->head];result=packet.result;
            if (packet.length) std::memcpy(data,packet.bytes.data(),packet.length);
            if (transferred) *transferred=packet.length;
            const auto age=std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now()-packet.queued).count();
            stats_.max_queue_age_ns=std::max(stats_.max_queue_age_ns,static_cast<uint64_t>(age));
            ring_->head=(ring_->head+1)%capacity;--ring_->count;
            if (!result) ++stats_.delivered;
        }
        const auto now=Clock::now();
        const bool report=now>=next_report_;
        const auto snapshot=stats_;
        const unsigned queued=ring_->count;
        if (report) next_report_=now+std::chrono::seconds(5);
        lock.unlock();
        if (report) std::fprintf(stderr,
            "SDK IQS USB queue: monotonic_ns=%lld queued=%u peak=%llu age_max_ns=%llu capacity=%u delivered=%llu failed=%llu\n",
            static_cast<long long>(std::chrono::duration_cast<std::chrono::nanoseconds>(now.time_since_epoch()).count()),
            queued,static_cast<unsigned long long>(snapshot.peak_queued_packets),
            static_cast<unsigned long long>(snapshot.max_queue_age_ns),capacity,
            static_cast<unsigned long long>(snapshot.delivered),static_cast<unsigned long long>(snapshot.failed));
        if (result) stop();
        return result;
    }
};
inline Pipeline& current() {static thread_local Pipeline pipeline(runtime_api());return pipeline;}
} // namespace websa_iqs_usb
