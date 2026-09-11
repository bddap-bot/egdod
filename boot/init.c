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
#include <time.h>
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

static void dhcp(const char *dev) {
    int up = socket(AF_INET, SOCK_DGRAM, 0);
    iface_up(up, "lo");
    iface_up(up, dev);
    close(up);
    mkdir("/etc", 0755);
    pid_t p = fork();
    if (p == 0) {
        char *av[] = { "/bin/busybox", "udhcpc", "-i", (char *)dev, "-n", "-q",
                       "-s", "/bin/udhcpc.script", NULL };
        execv(av[0], av);
        _exit(127);
    }
    int st;
    waitpid(p, &st, 0);
    if (st) printf("init: udhcpc exit status=%d\n", st);
}

static void network(void) {
    if (flag("egdod.dhcp")) {
        char *dev = "eth0";
        dhcp(dev);
        return;
    }
    const char *ip = arg("egdod.ip");
    if (!ip) return;
    char *dev = "eth0";
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

static pid_t spawn_agent(char **av) {
    pid_t p = fork();
    if (p == 0) {
        execv(av[0], av);
        printf("init: exec %s failed errno=%d\n", av[0], errno);
        _exit(127);
    }
    return p;
}

static void switch_root(pid_t *agent, char **av) {
    char req[256] = {0};
    int fd = open("/switch.req", O_RDONLY);
    if (fd < 0) return;
    read(fd, req, sizeof req - 1);
    close(fd);
    unlink("/switch.req");
    char newroot[256], initpath[256];
    strncpy(newroot, word_at(req, 0), sizeof newroot - 1);
    strncpy(initpath, word_at(req, 1), sizeof initpath - 1);
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
        *agent = spawn_agent(av);
        return;
    }
    chdir("/");
    char *nav[] = { initpath, NULL };
    execv(initpath, nav);
    printf("init: switch_root exec %s failed errno=%d\n", initpath, errno);
    for (;;) pause();
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
    char *ml = mods ? dup_word(mods) : strdup("/e1000.ko,/efivarfs.ko");
    for (char *tok = strtok(ml, ","); tok; tok = strtok(NULL, ",")) load_module(tok);

    mkdir("/sys/firmware", 0755);
    mkdir("/sys/firmware/efi", 0755);
    mkdir("/sys/firmware/efi/efivars", 0755);
    mount("efivarfs", "/sys/firmware/efi/efivars", "efivarfs", 0, NULL);

    network();

    setenv("PATH", "/bin", 1);
    if (access("/bin/busybox", X_OK) == 0) {
        pid_t bp = fork();
        if (bp == 0) {
            char *bv[] = { "/bin/busybox", "--install", "-s", "/bin", NULL };
            execv(bv[0], bv);
            _exit(127);
        }
        int bs;
        waitpid(bp, &bs, 0);
    }

    printf("init: egdod PID 1 up, launching agent\n");
    fflush(stdout);

    char **av = agent_argv();
    pid_t agent = spawn_agent(av);
    for (;;) {
        int st;
        pid_t w;
        while ((w = waitpid(-1, &st, WNOHANG)) > 0) {
            if (w == agent) {
                printf("init: agent exited; relaunching\n");
                fflush(stdout);
                agent = spawn_agent(av);
            }
        }
        if (access("/switch.req", F_OK) == 0) switch_root(&agent, av);
        struct timespec ts = { 1, 0 };
        nanosleep(&ts, NULL);
    }
}
