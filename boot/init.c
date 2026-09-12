#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <net/route.h>
#include <net/if.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define WIRED_WAIT 10
#define STATION_WAIT 30
#define AP_WAIT 15
#define DHCP_WAIT 20
#define RETRY_WAIT 30
#define NETWORKS "/wpa_supplicant.conf"

static char cmdline[8192];

static const char *arg(const char *key) {
    size_t kl = strlen(key);
    for (char *p = cmdline; p && *p;) {
        while (*p == ' ') p++;
        if (!strncmp(p, key, kl) && p[kl] == '=') return p + kl + 1;
        char *sp = strchr(p, ' ');
        p = sp ? sp + 1 : NULL;
    }
    return NULL;
}

static int flag(const char *key) {
    size_t kl = strlen(key);
    for (char *p = cmdline; p && *p;) {
        while (*p == ' ') p++;
        if (!strncmp(p, key, kl) && (p[kl] == ' ' || p[kl] == 0 || p[kl] == '\n')) return 1;
        char *sp = strchr(p, ' ');
        p = sp ? sp + 1 : NULL;
    }
    return 0;
}

static char *dup_word(const char *s) {
    size_t n = 0;
    while (s[n] && s[n] != ' ' && s[n] != '\n') n++;
    char *o = malloc(n + 1);
    memcpy(o, s, n);
    o[n] = 0;
    return o;
}

static int read_file(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) return -1;
    buf[n] = 0;
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == ' ')) buf[--n] = 0;
    return (int)n;
}

static pid_t spawn(char **av) {
    pid_t p = fork();
    if (p == 0) {
        execv(av[0], av);
        printf("init: exec %s failed errno=%d\n", av[0], errno);
        _exit(127);
    }
    if (p < 0) printf("init: fork failed errno=%d\n", errno);
    return p;
}

static int run(char **av) {
    pid_t p = spawn(av);
    if (p < 0) return -1;
    int st = 0;
    waitpid(p, &st, 0);
    return st;
}

static int capture(char **av, char *buf, size_t cap) {
    int p[2];
    if (pipe(p)) return -1;
    pid_t c = fork();
    if (c == 0) {
        dup2(p[1], 1);
        close(p[0]);
        close(p[1]);
        execv(av[0], av);
        _exit(127);
    }
    close(p[1]);
    if (c < 0) { close(p[0]); return -1; }
    size_t n = 0;
    ssize_t r;
    while (n < cap - 1 && (r = read(p[0], buf + n, cap - 1 - n)) > 0) n += r;
    buf[n] = 0;
    close(p[0]);
    int st;
    waitpid(c, &st, 0);
    return st ? -1 : (int)n;
}

static void stop(pid_t *p) {
    if (*p <= 0) return;
    kill(*p, SIGTERM);
    for (int i = 0; i < 20 && waitpid(*p, NULL, WNOHANG) == 0; i++) usleep(100000);
    if (waitpid(*p, NULL, WNOHANG) == 0) { kill(*p, SIGKILL); waitpid(*p, NULL, 0); }
    *p = -1;
}

static void modprobe(const char *name) {
    char *av[] = { "/bin/busybox", "modprobe", "-q", (char *)name, NULL };
    run(av);
}

static void coldplug(void) {
    static const char *buses[] = { "pci", "usb", "sdio", "platform", "virtio", NULL };
    for (const char **b = buses; *b; b++) {
        char dir[64];
        snprintf(dir, sizeof dir, "/sys/bus/%s/devices", *b);
        DIR *d = opendir(dir);
        if (!d) continue;
        struct dirent *e;
        while ((e = readdir(d))) {
            char path[512], alias[512];
            snprintf(path, sizeof path, "%s/%s/modalias", dir, e->d_name);
            if (read_file(path, alias, sizeof alias) > 0) modprobe(alias);
        }
        closedir(d);
    }
}

static void set_addr(const char *dev, const char *ip, const char *mask) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { printf("init: socket errno=%d\n", errno); return; }
    struct ifreq ifr;
    struct sockaddr_in *sin = (struct sockaddr_in *)&ifr.ifr_addr;
    unsigned long reqs[] = { SIOCSIFADDR, SIOCSIFNETMASK };
    const char *vals[] = { ip, mask };
    for (int i = 0; i < (mask ? 2 : 1); i++) {
        memset(&ifr, 0, sizeof ifr);
        strncpy(ifr.ifr_name, dev, IFNAMSIZ - 1);
        sin->sin_family = AF_INET;
        inet_pton(AF_INET, vals[i], &sin->sin_addr);
        if (ioctl(fd, reqs[i], &ifr)) printf("init: address %s/%s on %s errno=%d\n", ip, mask, dev, errno);
    }
    close(fd);
}

static void iface_up(const char *dev) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return;
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    strncpy(ifr.ifr_name, dev, IFNAMSIZ - 1);
    ioctl(fd, SIOCGIFFLAGS, &ifr);
    ifr.ifr_flags |= IFF_UP | IFF_RUNNING;
    if (ioctl(fd, SIOCSIFFLAGS, &ifr)) printf("init: %s up errno=%d\n", dev, errno);
    close(fd);
}

static void default_route(const char *dev, const char *gw) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return;
    struct rtentry rt;
    memset(&rt, 0, sizeof rt);
    struct sockaddr_in *d = (struct sockaddr_in *)&rt.rt_dst;
    struct sockaddr_in *m = (struct sockaddr_in *)&rt.rt_genmask;
    struct sockaddr_in *g = (struct sockaddr_in *)&rt.rt_gateway;
    d->sin_family = m->sin_family = g->sin_family = AF_INET;
    inet_pton(AF_INET, gw, &g->sin_addr);
    rt.rt_dev = (char *)dev;
    rt.rt_flags = RTF_UP | RTF_GATEWAY;
    if (ioctl(fd, SIOCADDRT, &rt)) printf("init: route via %s errno=%d\n", gw, errno);
    close(fd);
}

enum kind { NONE, WIRED, STATION, AP, BLE };

struct link {
    enum kind kind;
    char dev[IFNAMSIZ];
    char dial[64];
    pid_t daemon;
    pid_t dhcp;
    pid_t dbus;
    pid_t bluetoothd;
    struct stat networks;
};

static struct link up;
static char wireless[IFNAMSIZ];

static int has_addr(const char *dev) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return 0;
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    strncpy(ifr.ifr_name, dev, IFNAMSIZ - 1);
    int ok = ioctl(fd, SIOCGIFADDR, &ifr) == 0 && ((struct sockaddr_in *)&ifr.ifr_addr)->sin_addr.s_addr != 0;
    close(fd);
    return ok;
}

static void dhcp(void) {
    mkdir("/etc", 0755);
    char *av[] = { "/bin/busybox", "udhcpc", "-f", "-i", up.dev, "-s", "/bin/udhcpc.script", NULL };
    up.dhcp = spawn(av);
    for (int t = 0; t < DHCP_WAIT && up.dhcp > 0 && !has_addr(up.dev); t++) {
        if (waitpid(up.dhcp, NULL, WNOHANG) == up.dhcp) { up.dhcp = -1; break; }
        sleep(1);
    }
    if (!has_addr(up.dev)) printf("init: no lease on %s within %ds%s\n", up.dev, DHCP_WAIT, up.dhcp > 0 ? "; udhcpc keeps trying" : " and udhcpc exited");
}

static int is_wireless(const char *dev) {
    char p[320];
    snprintf(p, sizeof p, "/sys/class/net/%s/phy80211", dev);
    return access(p, F_OK) == 0;
}

static int carrier(const char *dev) {
    char p[320], v[8];
    snprintf(p, sizeof p, "/sys/class/net/%s/carrier", dev);
    return read_file(p, v, sizeof v) > 0 && v[0] == '1';
}

static int wait_wired(void) {
    for (int t = 0; t <= WIRED_WAIT; t++) {
        DIR *d = opendir("/sys/class/net");
        if (!d) return 0;
        struct dirent *e;
        int wired = 0;
        while ((e = readdir(d))) {
            if (e->d_name[0] == '.' || !strcmp(e->d_name, "lo")) continue;
            if (is_wireless(e->d_name)) {
                if (!wireless[0]) snprintf(wireless, sizeof wireless, "%s", e->d_name);
                continue;
            }
            wired++;
            iface_up(e->d_name);
            if (carrier(e->d_name)) {
                snprintf(up.dev, sizeof up.dev, "%s", e->d_name);
                closedir(d);
                return 1;
            }
        }
        closedir(d);
        if (!wired && wireless[0] && t >= 2) return 0;
        if (t == 0) printf("init: %d wired device(s), waiting up to %ds for a carrier\n", wired, WIRED_WAIT);
        sleep(1);
        coldplug();
    }
    return 0;
}

static int wired_ready(void) {
    coldplug();
    DIR *d = opendir("/sys/class/net");
    if (!d) return 0;
    struct dirent *e;
    int ready = 0;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.' || !strcmp(e->d_name, "lo") || is_wireless(e->d_name)) continue;
        iface_up(e->d_name);
        if (carrier(e->d_name)) { ready = 1; break; }
    }
    closedir(d);
    return ready;
}

static int wait_carrier(int secs) {
    for (int t = 0; t < secs; t++) {
        if (carrier(up.dev)) return 1;
        if (up.daemon > 0 && waitpid(up.daemon, NULL, WNOHANG) == up.daemon) { up.daemon = -1; return 0; }
        sleep(1);
    }
    return 0;
}

static int station(void) {
    if (!wireless[0] || access(NETWORKS, R_OK)) return 0;
    snprintf(up.dev, sizeof up.dev, "%s", wireless);
    iface_up(up.dev);
    char *av[] = { "/bin/wpa_supplicant", "-i", up.dev, "-c", NETWORKS, NULL };
    up.daemon = spawn(av);
    printf("init: %s scanning for a baked network, up to %ds\n", up.dev, STATION_WAIT);
    if (wait_carrier(STATION_WAIT)) return 1;
    printf("init: no baked network in range on %s\n", up.dev);
    stop(&up.daemon);
    return 0;
}

static int field(const char *text, const char *key, char *out, size_t cap) {
    size_t kl = strlen(key);
    for (const char *p = text; p && *p; p = strchr(p, '\n') ? strchr(p, '\n') + 1 : NULL) {
        if (strncmp(p, key, kl) || p[kl] != '=') continue;
        const char *v = p + kl + 1;
        size_t n = strcspn(v, "\n");
        if (n >= cap) return 0;
        memcpy(out, v, n);
        out[n] = 0;
        return 1;
    }
    return 0;
}

static int access_point(void) {
    const char *id = arg("egdod.controller");
    if (!wireless[0] || !id) return 0;
    snprintf(up.dev, sizeof up.dev, "%s", wireless);
    char *idw = dup_word(id);
    char text[1024], conf[1024], ssid[64], target[32], controller[32], port[8], prefix[4];
    char *lav[] = { "/egdod", "link", idw, NULL };
    char *hav[] = { "/egdod", "link", idw, "--hostapd", up.dev, NULL };
    if (capture(lav, text, sizeof text) < 0 || capture(hav, conf, sizeof conf) < 0) {
        printf("init: egdod link %s failed\n", idw);
        return 0;
    }
    if (!field(text, "ssid", ssid, sizeof ssid) || !field(text, "target", target, sizeof target)
        || !field(text, "controller", controller, sizeof controller) || !field(text, "port", port, sizeof port)
        || !field(text, "prefix", prefix, sizeof prefix)) {
        printf("init: egdod link printed an incomplete derivation\n");
        return 0;
    }
    int fd = open("/hostapd.conf", O_WRONLY | O_CREAT | O_TRUNC, 0600);
    ssize_t wrote = fd < 0 ? -1 : write(fd, conf, strlen(conf));
    if (fd >= 0) close(fd);
    if (wrote != (ssize_t)strlen(conf)) {
        printf("init: writing /hostapd.conf errno=%d\n", errno);
        return 0;
    }
    snprintf(up.dial, sizeof up.dial, "%s:%s", controller, port);
    struct in_addr m = { htonl(~0u << (32 - atoi(prefix))) };
    iface_up(up.dev);
    set_addr(up.dev, target, inet_ntoa(m));
    char *av[] = { "/bin/hostapd", "/hostapd.conf", NULL };
    up.daemon = spawn(av);
    if (!wait_carrier(AP_WAIT)) {
        printf("init: hostapd did not bring %s up\n", up.dev);
        stop(&up.daemon);
        return 0;
    }
    if (stat(NETWORKS, &up.networks)) memset(&up.networks, 0, sizeof up.networks);
    printf("init: link access-point %s on %s as %s; the controller dials in at %s\n", ssid, up.dev, target, up.dial);
    return 1;
}

static int bluetooth(void) {
    const char *id = arg("egdod.controller");
    if (!id || access("/bin/bluetoothd", X_OK) || access("/bin/dbus-daemon", X_OK)) return 0;
    if (up.dev[0]) set_addr(up.dev, "0.0.0.0", NULL);
    up.dev[0] = 0;
    mkdir("/run", 0755);
    mkdir("/run/dbus", 0755);
    mkdir("/var", 0755);
    mkdir("/var/lib", 0755);
    mkdir("/var/lib/bluetooth", 0700);
    char uuid[64];
    if (read_file("/proc/sys/kernel/random/uuid", uuid, sizeof uuid) > 0) {
        char machine[33];
        int n = 0;
        for (int i = 0; uuid[i] && n < 32; i++) if (uuid[i] != '-') machine[n++] = uuid[i];
        machine[n] = 0;
        int fd = open("/etc/machine-id", O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (fd >= 0) { write(fd, machine, n); close(fd); }
    }
    char *dav[] = { "/bin/dbus-daemon", "--config-file=/etc/dbus-1/system.conf", "--nofork", NULL };
    up.dbus = spawn(dav);
    sleep(1);
    char *bav[] = { "/bin/bluetoothd", "--nodetach", "--experimental", NULL };
    up.bluetoothd = spawn(bav);
    sleep(1);
    char *idw = dup_word(id);
    char *av[] = { "/egdod", "ble", "--controller", idw, "--key-file", "/agent.key", "--output", NETWORKS, NULL };
    up.daemon = spawn(av);
    if (stat(NETWORKS, &up.networks)) memset(&up.networks, 0, sizeof up.networks);
    printf("init: link BLE first hop; waiting for network credentials\n");
    return 1;
}

static int networks_changed(void) {
    struct stat st;
    if (stat(NETWORKS, &st)) return 0;
    return st.st_ino != up.networks.st_ino || st.st_mtim.tv_sec != up.networks.st_mtim.tv_sec
        || st.st_mtim.tv_nsec != up.networks.st_mtim.tv_nsec;
}

static void bring_down(void) {
    stop(&up.daemon);
    stop(&up.bluetoothd);
    stop(&up.dbus);
    if (up.dhcp > 0) { kill(up.dhcp, SIGTERM); up.dhcp = -1; }
    if (up.dev[0]) set_addr(up.dev, "0.0.0.0", NULL);
    memset(&up, 0, sizeof up);
    up.daemon = up.dhcp = up.dbus = up.bluetoothd = -1;
}

static void bring_up(void) {
    bring_down();
    coldplug();
    if (wait_wired()) {
        up.kind = WIRED;
        printf("init: link wired %s\n", up.dev);
        const char *ip = arg("egdod.ip");
        if (!ip) { dhcp(); return; }
        set_addr(up.dev, dup_word(ip), arg("egdod.mask") ? dup_word(arg("egdod.mask")) : "255.255.255.0");
        if (arg("egdod.gw")) default_route(up.dev, dup_word(arg("egdod.gw")));
        return;
    }
    if (station()) {
        up.kind = STATION;
        printf("init: link station %s\n", up.dev);
        dhcp();
        return;
    }
    if (access_point()) {
        up.kind = AP;
        return;
    }
    if (bluetooth()) {
        up.kind = BLE;
        return;
    }
    printf("init: no link%s\n", wireless[0] ? "" : " and no wireless device");
}

static char **agent_argv(void) {
    static char *av[16];
    int n = 0;
    av[n++] = "/egdod";
    av[n++] = "agent";
    const char *c = arg("egdod.controller");
    av[n++] = "--controller";
    av[n++] = c ? dup_word(c) : "";
    av[n++] = "--key-file";
    av[n++] = "/agent.key";
    if (up.kind == AP) {
        av[n++] = "--no-relay";
        av[n++] = "--direct";
        av[n++] = up.dial;
    } else {
        if (flag("egdod.norelay")) av[n++] = "--no-relay";
        if (arg("egdod.relay")) { av[n++] = "--relay"; av[n++] = dup_word(arg("egdod.relay")); }
        if (arg("egdod.direct")) { av[n++] = "--direct"; av[n++] = dup_word(arg("egdod.direct")); }
    }
    av[n] = NULL;
    return av;
}

static char *word_at(const char *s, int idx) {
    static char buf[256];
    for (int i = 0; i < idx; i++) {
        while (*s == ' ') s++;
        while (*s && *s != ' ') s++;
    }
    while (*s == ' ') s++;
    int n = 0;
    while (s[n] && s[n] != ' ' && s[n] != '\n' && n < 255) { buf[n] = s[n]; n++; }
    buf[n] = 0;
    return buf;
}

static void switch_root(pid_t *agent, char **av) {
    char req[256] = {0};
    int got = read_file("/switch.req", req, sizeof req);
    unlink("/switch.req");
    if (got < 0) { printf("init: /switch.req unreadable errno=%d; ignored\n", errno); return; }
    char newroot[256], initpath[256];
    snprintf(newroot, sizeof newroot, "%s", word_at(req, 0));
    snprintf(initpath, sizeof initpath, "%s", word_at(req, 1));
    if (!newroot[0]) strcpy(newroot, "/newroot");
    if (!initpath[0]) strcpy(initpath, "/sbin/init");
    char probe[512];
    snprintf(probe, sizeof probe, "%s%s", newroot, initpath);
    if (access(probe, X_OK)) {
        printf("init: switch_root refused, %s not executable errno=%d\n", probe, errno);
        return;
    }
    printf("init: switch_root into %s exec %s\n", newroot, initpath);
    fflush(stdout);
    if (*agent > 0) { kill(*agent, SIGKILL); int s; waitpid(*agent, &s, 0); *agent = -1; }
    if (chdir(newroot) || mount(".", "/", NULL, MS_MOVE, NULL) || chroot(".")) {
        printf("init: switch_root failed errno=%d; relaunching agent\n", errno);
        *agent = spawn(av);
        return;
    }
    chdir("/");
    char *nav[] = { initpath, NULL };
    execv(initpath, nav);
    printf("init: switch_root exec %s failed errno=%d\n", initpath, errno);
    for (;;) pause();
}

static void relink(pid_t *agent, char ***av) {
    if (*agent > 0) { kill(*agent, SIGKILL); waitpid(*agent, NULL, 0); }
    bring_up();
    *av = agent_argv();
    *agent = up.kind == BLE ? -1 : spawn(*av);
    fflush(stdout);
}

int main(void) {
    mkdir("/proc", 0755);
    mkdir("/sys", 0755);
    mkdir("/dev", 0755);
    mount("proc", "/proc", "proc", 0, NULL);
    mount("sysfs", "/sys", "sysfs", 0, NULL);
    mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
    read_file("/proc/cmdline", cmdline, sizeof cmdline);
    if (!arg("egdod.controller")) printf("init: no egdod.controller on the command line; this image cannot dial anyone\n");

    setenv("PATH", "/bin", 1);
    mkdir("/sbin", 0755);
    char *bv[] = { "/bin/busybox", "--install", "-s", "/bin", NULL };
    run(bv);
    char *sv[] = { "/bin/busybox", "--install", "-s", "/sbin", NULL };
    run(sv);

    const char *mods = arg("egdod.mods");
    char *ml = mods ? dup_word(mods) : strdup("");
    for (char *tok = strtok(ml, ","); tok; tok = strtok(NULL, ",")) modprobe(tok);
    mkdir("/sys/firmware", 0755);
    mkdir("/sys/firmware/efi", 0755);
    mkdir("/sys/firmware/efi/efivars", 0755);
    mount("efivarfs", "/sys/firmware/efi/efivars", "efivarfs", 0, NULL);

    iface_up("lo");
    up.daemon = up.dhcp = up.dbus = up.bluetoothd = -1;
    bring_up();

    printf("init: egdod PID 1 up%s\n", up.kind == BLE ? "; provisioning before the IP agent starts" : ", launching agent");
    fflush(stdout);

    char **av = agent_argv();
    pid_t agent = up.kind == BLE ? -1 : spawn(av);
    time_t retry_at = time(NULL) + RETRY_WAIT;
    for (;;) {
        int st;
        pid_t w;
        int lost_link = 0;
        int provisioned = 0;
        if (agent <= 0 && up.kind != BLE) agent = spawn(av);
        while ((w = waitpid(-1, &st, WNOHANG)) > 0) {
            if (w == agent) {
                printf("init: agent exited; relaunching\n");
                fflush(stdout);
                agent = spawn(av);
            } else if (w == up.daemon) {
                up.daemon = -1;
                if (up.kind == BLE && ((WIFEXITED(st) && WEXITSTATUS(st) == 0) || networks_changed())) provisioned = 1;
                else lost_link = 1;
            } else if (w == up.dhcp) {
                up.dhcp = -1;
            } else if (w == up.bluetoothd || w == up.dbus) {
                lost_link = 1;
            }
        }
        if (access("/switch.req", F_OK) == 0) {
            switch_root(&agent, av);
        } else if (up.kind == AP && networks_changed()) {
            printf("init: network credentials received over the access point\n");
            relink(&agent, &av);
        } else if (provisioned) {
            printf("init: BLE provisioning complete\n");
            relink(&agent, &av);
        } else if (lost_link) {
            printf("init: link daemon exited; bringing the link up again\n");
            relink(&agent, &av);
        } else if (up.kind == BLE && time(NULL) >= retry_at) {
            retry_at = time(NULL) + RETRY_WAIT;
            if (wired_ready()) {
                printf("init: wired carrier appeared; leaving BLE provisioning\n");
                relink(&agent, &av);
            }
        } else if (time(NULL) >= retry_at
                   && (up.kind == NONE || (up.dev[0] && !carrier(up.dev)))) {
            printf("init: %s; bringing the link up again\n", up.kind == NONE ? "no link" : "carrier lost");
            relink(&agent, &av);
            retry_at = time(NULL) + RETRY_WAIT;
        }
        struct timespec ts = { 1, 0 };
        nanosleep(&ts, NULL);
    }
}
