/* The xfstests build's generated config.h, hand-written for the fsx gate.
 *
 * fsx.c includes <config.h> through src/global.h, and xfstests only generates that header by
 * running its autoconf ./configure, which the tarball does not ship. This file declares only the
 * feature macros fsx.c and src/global.h branch on, for Linux glibc.
 *
 * Every macro left undefined is a capability the gate does not claim. The gate records the
 * binary's own usage text as evidence of what was actually compiled in, so these choices are
 * visible rather than implied:
 *
 * HAVE_LINUX_FALLOC_H enables fallocate() and with it the -F/-H/-z/-Y flags and the fallocate,
 *   punch_hole, zero_range and write_zeroes operations, which is the hole coverage this gate
 *   requires.
 * HAVE_SYS_PARAM_H is what supplies MIN() for the post-eof pollution path, not a macOS hint.
 *
 * Undefined on purpose:
 *   AIO and URING        libaio and liburing are not linked, so the binary omits -A and -U.
 *   HAVE_COPY_FILE_RANGE the range-clone family is not part of this gate.
 *   HAVE_XFS_*           no xfsprogs headers, so no xfs ioctls and no -x preallocation.
 *   HAVE_*_ATTRIBUTES_H  extended attributes are not part of this gate's coverage.
 */

#define HAVE_ASSERT_H 1
#define HAVE_DIRENT_H 1
#define HAVE_ERRNO_H 1
#define HAVE_ERR_H 1
#define HAVE_LIBGEN_H 1
#define HAVE_LINUX_FALLOC_H 1
#define HAVE_MALLOC_H 1
#define HAVE_PARAM_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_SYS_FCNTL_H 1
#define HAVE_SYS_IOCTL_H 1
#define HAVE_SYS_MMAN_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_STATVFS_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_SYS_WAIT_H 1
#define HAVE_TIME_H 1
#define HAVE_UNISTD_H 1
