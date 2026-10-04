// A bare RDP server that does nothing but krdp's Audio (src/Audio.cpp from audio.patch), for
// audio-loopback.sh. It listens on one port, takes one client, runs krdp's channel loop (the part of
// RdpConnection::run that matters here) and calls Audio::update() in it, so what's tested is the real
// Audio code against a real FreeRDP client. No video, no input, no krdp session.
//   audio-server <port> <cert.pem> <key.pem> <seconds>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <freerdp/channels/channels.h>
#include <freerdp/channels/drdynvc.h>
#include <freerdp/channels/wtsvc.h>
#include <freerdp/freerdp.h>
#include <freerdp/peer.h>

#include "Audio.h"
#include "PeerContext_p.h"
#include "RdpConnection.h"

static rdpContext *g_context;

// The one RdpConnection member Audio uses. The object itself is never made.
rdpContext *KRdp::RdpConnection::rdpPeerContext() const
{
    return g_context;
}

BOOL newPeerContext(freerdp_peer *peer, rdpContext *context)
{
    auto pc = reinterpret_cast<KRdp::PeerContext *>(context);
    pc->virtualChannelManager = WTSOpenServerA((LPSTR)peer->context);
    return pc->virtualChannelManager != nullptr;
}

void freePeerContext(freerdp_peer *, rdpContext *context)
{
    auto pc = reinterpret_cast<KRdp::PeerContext *>(context);
    if (pc) {
        WTSCloseServer(pc->virtualChannelManager);
    }
}

static BOOL yes(freerdp_peer *)
{
    return TRUE;
}

int main(int argc, char **argv)
{
    if (argc < 5) {
        return 2;
    }
    const int seconds = atoi(argv[4]);
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    sockaddr_in a{};
    a.sin_family = AF_INET;
    a.sin_port = htons(atoi(argv[1]));
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(ls, (sockaddr *)&a, sizeof a) || listen(ls, 1)) {
        perror("listen");
        return 1;
    }
    printf("listening\n");
    fflush(stdout);
    int fd = accept(ls, nullptr, nullptr);

    WTSRegisterWtsApiFunctionTable(FreeRDP_InitWtsApi()); // as krdp's Server.cpp does
    freerdp_peer *peer = freerdp_peer_new(fd);
    peer->ContextSize = sizeof(KRdp::PeerContext);
    peer->ContextNew = (psPeerContextNew)newPeerContext;
    peer->ContextFree = (psPeerContextFree)freePeerContext;
    if (!freerdp_peer_context_new(peer)) {
        return 1;
    }
    g_context = peer->context;
    auto pc = reinterpret_cast<KRdp::PeerContext *>(peer->context);
    rdpSettings *s = peer->context->settings;
    freerdp_settings_set_pointer_len(s, FreeRDP_RdpServerCertificate, freerdp_certificate_new_from_file(argv[2]), 1);
    freerdp_settings_set_pointer_len(s, FreeRDP_RdpServerRsaKey, freerdp_key_new_from_file(argv[3]), 1);
    freerdp_settings_set_bool(s, FreeRDP_RdpSecurity, false);
    freerdp_settings_set_bool(s, FreeRDP_TlsSecurity, true);
    freerdp_settings_set_bool(s, FreeRDP_NlaSecurity, false);
    freerdp_settings_set_bool(s, FreeRDP_SupportDynamicChannels, true);
    freerdp_settings_set_uint32(s, FreeRDP_ColorDepth, 32);
    freerdp_settings_set_bool(s, FreeRDP_AudioPlayback, true); // as krdp sets it (RdpConnection.cpp)
    peer->Capabilities = yes;
    peer->PostConnect = yes;
    peer->Activate = yes;
    if (!peer->Initialize(peer)) {
        return 1;
    }

    // The same as RdpConnection::run's handling of the channel manager
    KRdp::Audio audio(reinterpret_cast<KRdp::RdpConnection *>(&g_context));
    HANDLE channelEvent = WTSVirtualChannelManagerGetEventHandle(pc->virtualChannelManager);
    const auto end = std::chrono::steady_clock::now() + std::chrono::seconds(seconds);
    while (std::chrono::steady_clock::now() < end) {
        HANDLE events[32] = {channelEvent};
        const DWORD n = peer->GetEventHandles(peer, events + 1, 31);
        if (n == 0) {
            break;
        }
        WaitForMultipleObjects(1 + n, events, FALSE, 200);
        if (peer->CheckFileDescriptor(peer) != TRUE) {
            break;
        }
        if (peer->connected
            && WTSVirtualChannelManagerIsChannelJoined(pc->virtualChannelManager, DRDYNVC_SVC_CHANNEL_NAME)
            && WTSVirtualChannelManagerGetDrdynvcState(pc->virtualChannelManager) == DRDYNVC_STATE_NONE) {
            WTSVirtualChannelManagerOpen(pc->virtualChannelManager);
        }
        if (WTSVirtualChannelManagerCheckFileDescriptorEx(pc->virtualChannelManager, FALSE) != TRUE) {
            break;
        }
        if (peer->connected) {
            audio.update();
        }
    }
    audio.close();
    printf("closed\n");
    peer->Close(peer);
    freerdp_peer_context_free(peer);
    freerdp_peer_free(peer);
    return 0;
}
