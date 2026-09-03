// SPDX-License-Identifier: MIT OR GPL-2.0-only
//
// The C++ side of the C surface in devourer_shim.h.
//
// Nothing here is clever. It owns the four things devourer needs kept alive
// together - the libusb context, the claimed handle, the exclusive adapter
// lock and the device - in one struct with one destruction order, and it
// turns C++ exceptions into return codes, because letting one unwind through
// a Rust frame is undefined behaviour.

#include "devourer_shim.h"

#include <atomic>
#include <cstdio>
#include <cstring>
#include <memory>
#include <string>
#include <thread>

#include <libusb.h>

#include "IRtlDevice.h"
#include "RxPacket.h"
#include "SelectedChannel.h"
#include "UsbDeviceLock.h"
#include "UsbOpen.h"
#include "WiFiDriver.h"
#include "logger.h"

namespace {

/* USB product ids of the Realtek adapters this project's build of devourer
 * can drive. A dongle outside the list is still reachable by passing its
 * vendor and product id explicitly, which is what OEM-rebadged sticks
 * (TP-Link, COMFAST, LB-LINK) need - they enumerate under their own vendor
 * id rather than Realtek's. */
constexpr uint16_t kRealtekVid = 0x0bda;
constexpr uint16_t kRealtekPids[] = {
    0x8812,                         /* RTL8812AU, and some RTL8811AU boards */
    0x0811, 0xa811, 0xb811,         /* RTL8811AU / RTL8821AU cuts */
    0x8813,                         /* RTL8814AU */
    0x881a, 0x881b, 0x881c, 0xa81a, /* RTL8812AU-VS and RTL8812EU */
    0xe822, 0xa82a,                 /* RTL8822EU */
    0xb812, 0xb82c,                 /* RTL8822BU */
    0xc82c, 0xc82e, 0xc812,         /* RTL8822CU / RTL8812CU */
    0xc811,                         /* RTL8811CU / RTL8821CU */
    0xf72b, 0xb733,                 /* RTL8731BU / RTL8733BU */
};

void set_error(char *err, size_t len, const std::string &message) {
    if (err == nullptr || len == 0) {
        return;
    }
    std::snprintf(err, len, "%s", message.c_str());
}

ChannelWidth_t to_width(uint8_t width) {
    switch (width) {
    case DV_WIDTH_40:
        return CHANNEL_WIDTH_40;
    case DV_WIDTH_80:
        return CHANNEL_WIDTH_80;
    default:
        return CHANNEL_WIDTH_20;
    }
}

SelectedChannel to_channel(uint8_t channel, uint8_t width, uint8_t offset) {
    SelectedChannel selected{};
    selected.Channel = channel;
    selected.ChannelOffset = offset;
    selected.ChannelWidth = to_width(width);
    return selected;
}

} // namespace

struct dv_device {
    /* Declared in the order they must die in: the receive thread first, then
     * the device, the claim, the handle and finally the context. Getting that
     * backwards is a use-after-free that only shows itself on unplug. */
    Logger_t logger;
    libusb_context *ctx = nullptr;
    libusb_device_handle *handle = nullptr;
    std::shared_ptr<devourer::UsbDeviceLock> lock;
    std::unique_ptr<IRtlDevice> device;

    std::thread rx_thread;
    std::atomic<bool> running{false};
    std::string rx_error;

    dv_rx_callback callback = nullptr;
    void *user = nullptr;
    std::string chip_name;

    ~dv_device() {
        if (device) {
            device->StopRxLoop();
        }
        if (rx_thread.joinable()) {
            rx_thread.join();
        }
        device.reset();
        lock.reset();
        if (handle != nullptr) {
            libusb_close(handle);
        }
        if (ctx != nullptr) {
            libusb_exit(ctx);
        }
    }
};

namespace {

/* Open the first adapter matching the vendor and product ids, or the first
 * whose id is in the built-in list when none is given. */
libusb_device_handle *find_adapter(libusb_context *ctx, uint16_t vid,
                                   uint16_t pid, std::string &error) {
    libusb_device **list = nullptr;
    ssize_t count = libusb_get_device_list(ctx, &list);
    if (count < 0) {
        error = std::string("cannot list USB devices: ") +
                libusb_error_name(static_cast<int>(count));
        return nullptr;
    }

    libusb_device_handle *handle = nullptr;
    int last_error = 0;
    for (ssize_t i = 0; i < count && handle == nullptr; i++) {
        libusb_device_descriptor desc{};
        if (libusb_get_device_descriptor(list[i], &desc) != 0) {
            continue;
        }

        bool wanted = false;
        if (vid != 0 && pid != 0) {
            wanted = desc.idVendor == vid && desc.idProduct == pid;
        } else if (desc.idVendor == kRealtekVid) {
            for (uint16_t known : kRealtekPids) {
                wanted = wanted || desc.idProduct == known;
            }
        }
        if (!wanted) {
            continue;
        }

        int rc = libusb_open(list[i], &handle);
        if (rc != 0) {
            last_error = rc;
            handle = nullptr;
        }
    }
    libusb_free_device_list(list, 1);

    if (handle == nullptr) {
        if (last_error != 0) {
            /* Found one and could not open it. On Linux that is almost always
             * permissions, which is worth saying rather than "not found". */
            error = std::string("cannot open the adapter: ") +
                    libusb_error_name(last_error);
        } else {
            error = "no supported Realtek adapter found";
        }
    }
    return handle;
}

/* The shared tail of both open paths: claim the Wi-Fi interface and build
 * the device for whatever chip is behind the handle. */
dv_device *finish_open(std::unique_ptr<dv_device> dev, bool allow_reset,
                       char *err, size_t err_len) {
    try {
        int iface = devourer::find_wifi_interface(dev->handle);
        int rc = devourer::claim_interface_then_reset(
            dev->handle, iface, dev->logger, allow_reset, dev->lock);
        if (rc != 0) {
            set_error(err, err_len,
                      rc == LIBUSB_ERROR_BUSY
                          ? "the adapter is already in use by another program"
                          : std::string("cannot claim the adapter: ") +
                                libusb_error_name(rc));
            return nullptr;
        }

        WiFiDriver driver(dev->logger);
        dev->device = driver.CreateRtlDevice(dev->handle, dev->ctx, dev->lock);
        if (!dev->device) {
            set_error(err, err_len,
                      "this build has no driver for the chip in that adapter");
            return nullptr;
        }
        dev->chip_name = dev->device->GetAdapterCaps().chip_name;
    } catch (const std::exception &e) {
        set_error(err, err_len, e.what());
        return nullptr;
    } catch (...) {
        set_error(err, err_len, "the driver failed to open the adapter");
        return nullptr;
    }

    return dev.release();
}

} // namespace

extern "C" {

dv_device *dv_open_usb(uint16_t vid, uint16_t pid, char *err, size_t err_len) {
    auto dev = std::make_unique<dv_device>();
    dev->logger = std::make_shared<Logger>();
    /* The library's own diagnostics go to stderr; below a warning they are
     * per-frame and would drown everything else on a working link. */
    dev->logger->set_level(Logger::Level::Warn);

    int rc = libusb_init(&dev->ctx);
    if (rc != 0) {
        set_error(err, err_len,
                  std::string("cannot start libusb: ") + libusb_error_name(rc));
        return nullptr;
    }
    libusb_set_option(dev->ctx, LIBUSB_OPTION_LOG_LEVEL,
                      LIBUSB_LOG_LEVEL_WARNING);

    std::string error;
    dev->handle = find_adapter(dev->ctx, vid, pid, error);
    if (dev->handle == nullptr) {
        set_error(err, err_len, error);
        return nullptr;
    }

    return finish_open(std::move(dev), true, err, err_len);
}

dv_device *dv_open_fd(int fd, char *err, size_t err_len) {
    auto dev = std::make_unique<dv_device>();
    dev->logger = std::make_shared<Logger>();
    dev->logger->set_level(Logger::Level::Warn);

    /* Both options are global and must precede libusb_init. Without them
     * libusb tries to enumerate the bus, which an app sandbox does not permit
     * and which is not needed anyway: the descriptor already names the one
     * device this process is allowed to touch. */
    libusb_set_option(nullptr, LIBUSB_OPTION_NO_DEVICE_DISCOVERY);
    libusb_set_option(nullptr, LIBUSB_OPTION_WEAK_AUTHORITY);

    int rc = libusb_init(&dev->ctx);
    if (rc != 0) {
        set_error(err, err_len,
                  std::string("cannot start libusb: ") + libusb_error_name(rc));
        return nullptr;
    }

    rc = libusb_wrap_sys_device(dev->ctx, static_cast<intptr_t>(fd),
                                &dev->handle);
    if (rc != 0 || dev->handle == nullptr) {
        set_error(err, err_len, std::string("cannot adopt the USB handle: ") +
                                    libusb_error_name(rc));
        return nullptr;
    }

    /* No reset on this path. A USB reset re-enumerates the device, and the
     * descriptor the app was granted refers to the enumeration before it - it
     * would be left pointing at nothing, with no way to ask for another
     * without going back through the permission prompt. */
    return finish_open(std::move(dev), false, err, err_len);
}

int dv_start(dv_device *dev, uint8_t channel, uint8_t width, uint8_t offset,
             dv_rx_callback callback, void *user, char *err, size_t err_len) {
    if (dev == nullptr || dev->device == nullptr || callback == nullptr) {
        set_error(err, err_len, "no adapter to start");
        return -1;
    }
    if (dev->running.load()) {
        return 0;
    }

    dev->callback = callback;
    dev->user = user;
    dev->rx_error.clear();

    /* Bring-up happens here rather than on the receive thread so that a
     * failure - a chip that will not initialize, a channel the adapter cannot
     * tune - comes back as a return value instead of showing up later as
     * silence. InitWrite is exactly the bring-up half of Init. */
    try {
        dev->device->InitWrite(to_channel(channel, width, offset));
    } catch (const std::exception &e) {
        set_error(err, err_len, e.what());
        return -1;
    } catch (...) {
        set_error(err, err_len, "the adapter would not start");
        return -1;
    }

    dev->running.store(true);
    dev->rx_thread = std::thread([dev]() {
        try {
            dev->device->StartRxLoop([dev](const Packet &packet) {
                dv_packet out{};
                out.data = packet.Data.data();
                out.len = packet.Data.size();
                for (int i = 0; i < 4; i++) {
                    out.rssi[i] = packet.RxAtrib.rssi[i];
                    out.snr[i] = packet.RxAtrib.snr[i];
                }
                out.tsf = packet.RxAtrib.tsfl;
                out.rate = packet.RxAtrib.data_rate;
                out.bandwidth = packet.RxAtrib.bw;
                out.crc_error = packet.RxAtrib.crc_err;
                dev->callback(dev->user, &out);
            });
        } catch (const std::exception &e) {
            dev->rx_error = e.what();
        } catch (...) {
            dev->rx_error = "the receive loop stopped unexpectedly";
        }
        dev->running.store(false);
    });

    return 0;
}

int dv_set_channel(dv_device *dev, uint8_t channel, uint8_t width,
                   uint8_t offset, char *err, size_t err_len) {
    if (dev == nullptr || dev->device == nullptr) {
        set_error(err, err_len, "no adapter to tune");
        return -1;
    }
    try {
        dev->device->SetMonitorChannel(to_channel(channel, width, offset));
    } catch (const std::exception &e) {
        set_error(err, err_len, e.what());
        return -1;
    } catch (...) {
        set_error(err, err_len, "the adapter would not change channel");
        return -1;
    }
    return 0;
}

void dv_stop(dv_device *dev) {
    if (dev == nullptr || dev->device == nullptr) {
        return;
    }
    dev->device->StopRxLoop();
    if (dev->rx_thread.joinable()) {
        dev->rx_thread.join();
    }
    dev->running.store(false);
    /* Cleared only once the thread is joined: while it could still be running,
     * these two are what it calls. */
    dev->callback = nullptr;
    dev->user = nullptr;
}

void dv_close(dv_device *dev) {
    if (dev == nullptr) {
        return;
    }
    dv_stop(dev);
    delete dev;
}

const char *dv_chip_name(dv_device *dev) {
    if (dev == nullptr || dev->chip_name.empty()) {
        return "unknown";
    }
    return dev->chip_name.c_str();
}

bool dv_running(dv_device *dev) {
    return dev != nullptr && dev->running.load();
}

const char *dv_rx_error(dv_device *dev) {
    if (dev == nullptr || dev->rx_error.empty()) {
        return nullptr;
    }
    return dev->rx_error.c_str();
}

size_t dv_supported_count(void) {
    return sizeof(kRealtekPids) / sizeof(kRealtekPids[0]);
}

void dv_supported_id(size_t index, uint16_t *vid, uint16_t *pid) {
    if (index >= dv_supported_count()) {
        return;
    }
    if (vid != nullptr) {
        *vid = kRealtekVid;
    }
    if (pid != nullptr) {
        *pid = kRealtekPids[index];
    }
}

} // extern "C"
