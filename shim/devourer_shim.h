/* SPDX-License-Identifier: MIT OR GPL-2.0-only
 *
 * A C surface over devourer's C++ IRtlDevice, narrowed to what a wfb-ng
 * ground station needs: open an adapter, sit on a channel, and get every
 * frame that lands on it.
 *
 * Everything devourer can do beyond that - injection, hopping, beamforming,
 * spectrum work - is deliberately not here. A wider surface would be more
 * unsafe code to audit for no gain, and the parts that matter to a receiver
 * fit in eight functions.
 *
 * Threading: dv_open_* and dv_start run on the caller's thread and return
 * only when the adapter is up or has failed. dv_start then leaves one thread
 * of its own inside devourer's receive loop, and that thread is what calls
 * the packet callback. dv_stop asks it to leave and joins it, so no callback
 * can be in flight once dv_stop returns - which is what makes it safe for the
 * callback to borrow state the caller owns.
 */

#ifndef DEVOURER_SHIM_H
#define DEVOURER_SHIM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* An opened adapter. */
typedef struct dv_device dv_device;

/* One received 802.11 frame and what the chip knew about it. */
typedef struct {
    /* The frame including its trailing four-byte checksum. Valid only for
     * the duration of the callback. */
    const uint8_t *data;
    size_t len;
    /* Per-antenna signal in the chip's own units, and per-antenna SNR in dB.
     * Chains the adapter does not have read zero. */
    uint8_t rssi[4];
    int8_t snr[4];
    /* The chip's own microsecond clock when the frame arrived. */
    uint32_t tsf;
    /* Realtek rate code; 0x80 and above are HT, 0x100 and above VHT. */
    uint16_t rate;
    uint8_t bandwidth;
    /* Set when the frame failed its checksum. Delivered rather than dropped,
     * because the count is the earliest warning a marginal link gives. */
    bool crc_error;
} dv_packet;

typedef void (*dv_rx_callback)(void *user, const dv_packet *packet);

/* Channel widths, matching devourer's own ChannelWidth_t. */
#define DV_WIDTH_20 0
#define DV_WIDTH_40 1
#define DV_WIDTH_80 2

/* Open the first supported adapter, or the one matching `vid` and `pid` when
 * both are nonzero. The desktop path: this end owns the enumeration.
 *
 * Returns NULL on failure, with a message written into `err`.
 */
dv_device *dv_open_usb(uint16_t vid, uint16_t pid, char *err, size_t err_len);

/* Open the adapter behind an already-open USB file descriptor.
 *
 * The Android path. An app is handed a descriptor for a device the user has
 * granted it and has no way to enumerate anything, so device discovery is
 * switched off and the descriptor adopted as it is. The caller keeps
 * ownership of `fd` and must not close it before dv_close.
 */
dv_device *dv_open_fd(int fd, char *err, size_t err_len);

/* Bring the chip up on a channel and start receiving.
 *
 * `callback` is called on the receive thread for every frame, including
 * frames belonging to other networks - filtering is the caller's job.
 * Returns 0 on success, -1 with a message in `err`.
 */
int dv_start(dv_device *dev, uint8_t channel, uint8_t width, uint8_t offset,
             dv_rx_callback callback, void *user, char *err, size_t err_len);

/* Retune without restarting. Returns 0 on success. */
int dv_set_channel(dv_device *dev, uint8_t channel, uint8_t width,
                   uint8_t offset, char *err, size_t err_len);

/* Stop receiving and join the receive thread. Safe to call more than once,
 * and safe on a device that was never started. */
void dv_stop(dv_device *dev);

/* Stop if needed, then release the adapter. */
void dv_close(dv_device *dev);

/* The chip behind an open device, e.g. "RTL8812A". Valid until dv_close. */
const char *dv_chip_name(dv_device *dev);

/* Whether the receive thread is still running. It exits on its own if the
 * adapter is unplugged, which is the one failure the caller cannot see any
 * other way: frames simply stop arriving. */
bool dv_running(dv_device *dev);

/* The message the receive thread left if it exited on an error, or NULL. */
const char *dv_rx_error(dv_device *dev);

/* The USB ids this build knows how to drive.
 *
 * dv_open_usb matches against these itself, but a caller that cannot
 * enumerate - an Android app, which is handed one device at a time - needs
 * the list to decide which device to ask for. Keeping it here means there is
 * one list rather than one per platform.
 *
 * Returns the number of entries; `index` below that fills `vid` and `pid`.
 */
size_t dv_supported_count(void);
void dv_supported_id(size_t index, uint16_t *vid, uint16_t *pid);

#ifdef __cplusplus
}
#endif

#endif /* DEVOURER_SHIM_H */
