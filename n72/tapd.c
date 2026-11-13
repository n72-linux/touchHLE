/*
 * n72-tapd: a virtual touch screen (uinput) for scripted app tests.
 *
 *   n72-tapd /tmp/n72-tap &         creates the device, then reads commands
 *   echo "tap 160 240" > /tmp/n72-tap
 *   echo "swipe 40 400 280 400 300" > /tmp/n72-tap   (x0 y0 x1 y1 ms)
 *   echo "hold 160 240 800" > /tmp/n72-tap           (x y ms)
 *
 * Coordinates are panel pixels (320 x 480, portrait). SDL's evdev backend
 * sees a direct multitouch device like the real one.
 */
#include <fcntl.h>
#include <linux/uinput.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int fd;

static void ev(int type, int code, int value)
{
	struct input_event e;

	memset(&e, 0, sizeof(e));
	e.type = type;
	e.code = code;
	e.value = value;
	if (write(fd, &e, sizeof(e)) != sizeof(e))
		perror("write");
}

static void msleep(int ms)
{
	struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };

	nanosleep(&ts, NULL);
}

static void down(int x, int y, int first)
{
	if (first) {
		ev(EV_ABS, ABS_MT_SLOT, 0);
		ev(EV_ABS, ABS_MT_TRACKING_ID, 1);
	}
	ev(EV_ABS, ABS_MT_POSITION_X, x);
	ev(EV_ABS, ABS_MT_POSITION_Y, y);
	ev(EV_ABS, ABS_X, x);
	ev(EV_ABS, ABS_Y, y);
	if (first)
		ev(EV_KEY, BTN_TOUCH, 1);
	ev(EV_SYN, SYN_REPORT, 0);
}

static void up(void)
{
	ev(EV_ABS, ABS_MT_SLOT, 0);
	ev(EV_ABS, ABS_MT_TRACKING_ID, -1);
	ev(EV_KEY, BTN_TOUCH, 0);
	ev(EV_SYN, SYN_REPORT, 0);
}

static void absax(int code, int max)
{
	struct uinput_abs_setup a;

	memset(&a, 0, sizeof(a));
	a.code = code;
	a.absinfo.maximum = max;
	ioctl(fd, UI_ABS_SETUP, &a);
}

int main(int argc, char **argv)
{
	const char *fifo = argc > 1 ? argv[1] : "/tmp/n72-tap";
	struct uinput_setup us;
	char line[128];

	fd = open("/dev/uinput", O_WRONLY);
	if (fd < 0) {
		perror("/dev/uinput");
		return 1;
	}
	ioctl(fd, UI_SET_EVBIT, EV_KEY);
	ioctl(fd, UI_SET_KEYBIT, BTN_TOUCH);
	ioctl(fd, UI_SET_EVBIT, EV_ABS);
	ioctl(fd, UI_SET_ABSBIT, ABS_X);
	ioctl(fd, UI_SET_ABSBIT, ABS_Y);
	ioctl(fd, UI_SET_ABSBIT, ABS_MT_SLOT);
	ioctl(fd, UI_SET_ABSBIT, ABS_MT_TRACKING_ID);
	ioctl(fd, UI_SET_ABSBIT, ABS_MT_POSITION_X);
	ioctl(fd, UI_SET_ABSBIT, ABS_MT_POSITION_Y);
	ioctl(fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT);
	absax(ABS_X, 319);
	absax(ABS_Y, 479);
	absax(ABS_MT_POSITION_X, 319);
	absax(ABS_MT_POSITION_Y, 479);
	absax(ABS_MT_SLOT, 1);
	absax(ABS_MT_TRACKING_ID, 65535);
	memset(&us, 0, sizeof(us));
	us.id.bustype = BUS_VIRTUAL;
	us.id.vendor = 0x7272;
	us.id.product = 1;
	strcpy(us.name, "n72-tapd virtual touchscreen");
	ioctl(fd, UI_DEV_SETUP, &us);
	if (ioctl(fd, UI_DEV_CREATE) < 0) {
		perror("UI_DEV_CREATE");
		return 1;
	}
	unlink(fifo);
	if (mkfifo(fifo, 0600)) {
		perror(fifo);
		return 1;
	}
	for (;;) {
		FILE *f = fopen(fifo, "r");

		if (!f)
			return 1;
		while (fgets(line, sizeof(line), f)) {
			int x0, y0, x1, y1, ms;

			if (sscanf(line, "tap %d %d", &x0, &y0) == 2) {
				down(x0, y0, 1);
				msleep(80);
				up();
			} else if (sscanf(line, "hold %d %d %d", &x0, &y0, &ms) == 3) {
				down(x0, y0, 1);
				msleep(ms);
				up();
			} else if (sscanf(line, "swipe %d %d %d %d %d", &x0, &y0, &x1, &y1, &ms) == 5) {
				int i, n = ms / 16 > 1 ? ms / 16 : 2;

				for (i = 0; i <= n; i++) {
					down(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n, i == 0);
					msleep(16);
				}
				up();
			}
		}
		fclose(f);
	}
}
