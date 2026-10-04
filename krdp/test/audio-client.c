// A bare FreeRDP client with sound and microphone on, for audio-loopback.sh: the same settings
// cc-panels sets (rdpsnd and audin on their PulseAudio backends), without a window, a GPU or a
// picture. It connects, stays for a number of seconds and leaves.
//   audio-client <port> <seconds>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#include <freerdp/client.h>
#include <freerdp/freerdp.h>

static BOOL pre(freerdp *i)
{
    return TRUE;
}

static BOOL cnew(freerdp *i, rdpContext *c)
{
    i->PreConnect = pre;
    return TRUE;
}

int main(int argc, char **argv)
{
    RDP_CLIENT_ENTRY_POINTS ep = {0};
    ep.Version = RDP_CLIENT_INTERFACE_VERSION;
    ep.Size = sizeof(RDP_CLIENT_ENTRY_POINTS_V1);
    ep.ContextSize = sizeof(rdpClientContext);
    ep.ClientNew = cnew;
    rdpContext *c = freerdp_client_context_new(&ep);
    rdpSettings *s = c->settings;
    freerdp_settings_set_string(s, FreeRDP_ServerHostname, "127.0.0.1");
    freerdp_settings_set_string(s, FreeRDP_Username, "test");
    freerdp_settings_set_string(s, FreeRDP_Password, "test");
    freerdp_settings_set_uint32(s, FreeRDP_ServerPort, atoi(argv[1]));
    freerdp_settings_set_uint32(s, FreeRDP_ColorDepth, 32);
    freerdp_settings_set_bool(s, FreeRDP_NlaSecurity, FALSE);
    freerdp_settings_set_bool(s, FreeRDP_TlsSecurity, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_RdpSecurity, FALSE);
    freerdp_settings_set_bool(s, FreeRDP_AutoAcceptCertificate, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_DeactivateClientDecoding, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_SupportGraphicsPipeline, FALSE);
    freerdp_settings_set_bool(s, FreeRDP_AudioPlayback, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_AudioCapture, TRUE);
    if (freerdp_client_start(c) != 0 || !freerdp_connect(c->instance)) {
        fprintf(stderr, "can't connect (0x%08x)\n", freerdp_get_last_error(c));
        return 1;
    }
    printf("connected\n");
    fflush(stdout);
    time_t end = time(NULL) + atoi(argv[2]);
    HANDLE h[MAXIMUM_WAIT_OBJECTS];
    while (time(NULL) < end && !freerdp_shall_disconnect_context(c)) {
        DWORD n = freerdp_get_event_handles(c, h, MAXIMUM_WAIT_OBJECTS);
        if (n == 0 || WaitForMultipleObjects(n, h, FALSE, 200) == WAIT_FAILED || !freerdp_check_event_handles(c)) {
            break;
        }
    }
    freerdp_disconnect(c->instance);
    freerdp_client_stop(c);
    freerdp_client_context_free(c);
    printf("left\n");
    return 0;
}
