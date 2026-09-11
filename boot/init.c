#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <net/route.h>
#include <net/if.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

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

static void load_module(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return;
    long r = syscall(SYS_finit_module, fd, "", 0);
    if (r) printf("init: finit_module %s errno=%d\n", path, errno);
    close(fd);
}

static void set_addr(int fd, const char *dev, unsigned long req, const char *ip) {
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    strncpy(ifr.ifr_name, dev, IFNAMSIZ - 1);
    struct sockaddr_in *sin = (struct sockaddr_in *)&ifr.ifr_addr;
    sin->sin_family = AF_INET;
    inet_pton(AF_INET, ip, &sin->sin_addr);
    if (ioctl(fd, req, &ifr)) printf("init: ioctl %lu on %s errno=%d\n", req, dev, errno);
}

static void iface_up(int fd, const char *dev) {
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    strncpy(ifr.ifr_name, dev, IFNAMSIZ - 1);
    ioctl(fd, SIOCGIFFLAGS, &ifr);
    ifr.ifr_flags |= IFF_UP | IFF_RUNNING;
    if (ioctl(fd, SIOCSIFFLAGS, &ifr)) printf("init: %s up errno=%d\n", dev, errno);
}

static void default_route(int fd, const char *dev, const char *gw) {
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
}

static void network(void) {
    const char *ip = arg("egdod.ip");
    if (!ip) return;
    char *dev = arg("egdod.dev") ? dup_word(arg("egdod.dev")) : strdup("eth0");
    char *ipw = dup_word(ip);
    char *mask = arg("egdod.mask") ? dup_word(arg("egdod.mask")) : strdup("255.255.255.0");
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    iface_up(fd, "lo");
    set_addr(fd, dev, SIOCSIFADDR, ipw);
    set_addr(fd, dev, SIOCSIFNETMASK, mask);
    iface_up(fd, dev);
    if (arg("egdod.gw")) {
        char *gw = dup_word(arg("egdod.gw"));
        default_route(fd, dev, gw);
    }
    close(fd);
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
    if (flag("egdod.norelay")) av[n++] = "--no-relay";
    if (arg("egdod.relay")) { av[n++] = "--relay"; av[n++] = dup_word(arg("egdod.relay")); }
    if (arg("egdod.direct")) { av[n++] = "--direct"; av[n++] = dup_word(arg("egdod.direct")); }
    av[n] = NULL;
    return av;
}

int main(void) {
    mkdir("/proc", 0755);
    mkdir("/sys", 0755);
    mkdir("/dev", 0755);
    mount("proc", "/proc", "proc", 0, NULL);
    mount("sysfs", "/sys", "sysfs", 0, NULL);
    mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);

    int cf = open("/proc/cmdline", O_RDONLY);
    if (cf >= 0) {
        ssize_t n = read(cf, cmdline, sizeof cmdline - 1);
        if (n > 0) cmdline[n] = 0;
        close(cf);
    }

    const char *mods = arg("egdod.mods");
    char *ml = mods ? dup_word(mods) : strdup("/e1000.ko");
    for (char *tok = strtok(ml, ","); tok; tok = strtok(NULL, ",")) load_module(tok);

    network();

    printf("init: egdod PID 1 up, launching agent\n");
    fflush(stdout);

    char **av = agent_argv();
    for (;;) {
        pid_t pid = fork();
        if (pid == 0) {
            execv(av[0], av);
            printf("init: exec %s failed errno=%d\n", av[0], errno);
            _exit(127);
        }
        int status;
        for (;;) {
            pid_t w = wait(&status);
            if (w == pid) break;
            if (w < 0 && errno == ECHILD) break;
        }
        printf("init: agent exited; relaunching\n");
        fflush(stdout);
        sleep(2);
    }
}
