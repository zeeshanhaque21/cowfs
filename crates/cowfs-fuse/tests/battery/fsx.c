#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <errno.h>

#define MAXSZ (1 << 20)
static unsigned char shadow[MAXSZ];
static long fsize = 0;
static int fd;
static const char *path;

static void die(const char *m, long op, long a, long b) {
    printf("FAIL op=%ld %s a=%ld b=%ld\n", op, m, a, b);
    exit(1);
}
static void chk(long op, const char *n, off_t off, size_t len, unsigned char *got) {
    if (memcmp(got, shadow + off, len)) {
        size_t i = 0;
        while (got[i] == shadow[off + i]) i++;
        printf("FAIL op=%ld %s mismatch at %ld (off %ld len %zu)\n", op, n, (long)off + (long)i, (long)off, len);
        exit(1);
    }
}
int main(int argc, char **argv) {
    path = argv[1];
    long nops = atol(argv[2]);
    srand(argc > 3 ? atoi(argv[3]) : 1);
    fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) die("open", 0, errno, 0);
    static unsigned char buf[MAXSZ];
    long cnt[8] = {0};
    for (long op = 1; op <= nops; op++) {
        int k = rand() % 100;
        off_t off = rand() % (MAXSZ - 1);
        size_t len = 1 + rand() % 65536;
        if (off + len > MAXSZ) len = MAXSZ - off;
        if (k < 30) { /* write */
            for (size_t i = 0; i < len; i++) buf[i] = rand();
            if (pwrite(fd, buf, len, off) != (ssize_t)len) die("pwrite", op, off, len);
            if (off > fsize) memset(shadow + fsize, 0, off - fsize);
            memcpy(shadow + off, buf, len);
            if (off + (long)len > fsize) fsize = off + len;
            cnt[0]++;
        } else if (k < 55) { /* read */
            if (off >= fsize) { off = fsize ? rand() % fsize : 0; }
            ssize_t r = pread(fd, buf, len, off);
            long want = fsize - off < (long)len ? fsize - off : (long)len;
            if (want < 0) want = 0;
            if (r != want) die("pread short/long", op, r, want);
            chk(op, "pread", off, r, buf);
            cnt[1]++;
        } else if (k < 65) { /* truncate */
            off_t ns = rand() % MAXSZ;
            if (ftruncate(fd, ns)) die("ftruncate", op, errno, ns);
            if (ns > fsize) memset(shadow + fsize, 0, ns - fsize);
            fsize = ns;
            cnt[2]++;
        } else if (k < 80) { /* mmap read */
            if (!fsize) continue;
            if (off >= fsize) off = rand() % fsize;
            size_t l = len; if (off + (long)l > fsize) l = fsize - off;
            off_t po = off & ~4095L; size_t ml = l + (off - po);
            unsigned char *m = mmap(0, ml, PROT_READ, MAP_SHARED, fd, po);
            if (m == MAP_FAILED) die("mmap", op, errno, 0);
            chk(op, "mmap read", off, l, m + (off - po));
            munmap(m, ml);
            cnt[3]++;
        } else if (k < 92) { /* mmap write */
            if (!fsize) continue;
            if (off >= fsize) off = rand() % fsize;
            size_t l = len; if (off + (long)l > fsize) l = fsize - off;
            off_t po = off & ~4095L; size_t ml = l + (off - po);
            unsigned char *m = mmap(0, ml, PROT_READ | PROT_WRITE, MAP_SHARED, fd, po);
            if (m == MAP_FAILED) die("mmap w", op, errno, 0);
            for (size_t i = 0; i < l; i++) { unsigned char c = rand(); m[off - po + i] = c; shadow[off + i] = c; }
            if (rand() % 2 && msync(m, ml, MS_SYNC)) die("msync", op, errno, 0);
            munmap(m, ml);
            cnt[4]++;
        } else if (k < 96) { /* reopen */
            close(fd);
            fd = open(path, O_RDWR);
            if (fd < 0) die("reopen", op, errno, 0);
            cnt[5]++;
        } else { /* fstat + full compare */
            struct stat st;
            if (fstat(fd, &st)) die("fstat", op, errno, 0);
            if (st.st_size != fsize) die("size", op, st.st_size, fsize);
            long o = 0;
            while (o < fsize) {
                size_t l = fsize - o > MAXSZ ? MAXSZ : fsize - o;
                ssize_t r = pread(fd, buf, l, o);
                if (r <= 0) die("full read", op, r, o);
                chk(op, "full", o, r, buf);
                o += r;
            }
            cnt[6]++;
        }
    }
    printf("PASS ops=%ld writes=%ld reads=%ld truncs=%ld mapr=%ld mapw=%ld reopen=%ld full=%ld finalsize=%ld\n",
           nops, cnt[0], cnt[1], cnt[2], cnt[3], cnt[4], cnt[5], cnt[6], fsize);
    return 0;
}
