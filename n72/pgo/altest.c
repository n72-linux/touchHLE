/*
 * altest: OpenAL Soft's mixer through a loopback device, for comparing builds
 * of it (n72/pgo/README): the same sources, rendered the same way, must give
 * the same samples. Prints a checksum of every second and of the whole.
 * Settings as on the iPod (ALSOFT_CONF=n72/alsoft.conf): int16 stereo
 * 44.1 kHz, cubic resampler, no limiter, no dither.
 */
#include <AL/al.h>
#include <AL/alc.h>
#include <AL/alext.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define RATE 44100

static uint32_t seed = 12345;
static int16_t noise(void)
{
	seed = seed * 1103515245 + 12345;
	return (int16_t)(seed >> 16);
}

static uint64_t fnv(uint64_t h, const void *p, size_t n)
{
	const unsigned char *b = p;

	while (n--)
		h = (h ^ *b++) * 0x100000001b3ull;
	return h;
}

int main(int argc, char **argv)
{
	int seconds = argc > 1 ? atoi(argv[1]) : 6, i, s;
	LPALCLOOPBACKOPENDEVICESOFT open_loop;
	LPALCRENDERSAMPLESSOFT render;
	ALCdevice *dev;
	ALCcontext *ctx;
	ALuint buf[4], src[6];
	static int16_t pcm[4][RATE * 2];
	static int16_t out[1024 * 2];
	uint64_t all = 0xcbf29ce484222325ull;

	open_loop = (LPALCLOOPBACKOPENDEVICESOFT)alcGetProcAddress(NULL, "alcLoopbackOpenDeviceSOFT");
	render = (LPALCRENDERSAMPLESSOFT)alcGetProcAddress(NULL, "alcRenderSamplesSOFT");
	if (!open_loop || !render || !(dev = open_loop(NULL))) {
		fprintf(stderr, "no loopback device\n");
		return 1;
	}
	ALCint attrs[] = { ALC_FORMAT_CHANNELS_SOFT, ALC_STEREO_SOFT, ALC_FORMAT_TYPE_SOFT, ALC_SHORT_SOFT,
			   ALC_FREQUENCY, RATE, 0 };
	ctx = alcCreateContext(dev, attrs);
	if (!ctx || !alcMakeContextCurrent(ctx)) {
		fprintf(stderr, "no context\n");
		return 1;
	}
	/* a sine sweep at 22050 Hz, noise at 11025 Hz, a square wave at 44100 Hz (mono), a stereo chord */
	for (i = 0; i < RATE; i++) {
		double t = i / 22050.0;
		pcm[0][i] = (int16_t)(12000 * sin(2 * M_PI * (200 + 900 * t) * t));
		pcm[1][i] = noise() / 3;
		pcm[2][i] = ((i / 50) & 1) ? 6000 : -6000;
		pcm[3][2 * i] = (int16_t)(8000 * sin(2 * M_PI * 440 * i / RATE));
		pcm[3][2 * i + 1] = (int16_t)(8000 * sin(2 * M_PI * 554.37 * i / RATE));
	}
	alGenBuffers(4, buf);
	alBufferData(buf[0], AL_FORMAT_MONO16, pcm[0], RATE * 2, 22050);
	alBufferData(buf[1], AL_FORMAT_MONO16, pcm[1], 11025 * 2, 11025);
	alBufferData(buf[2], AL_FORMAT_MONO16, pcm[2], RATE * 2, RATE);
	alBufferData(buf[3], AL_FORMAT_STEREO16, pcm[3], RATE * 4, RATE);
	alGenSources(6, src);
	static const float pitch[6] = { 1.0f, 0.73f, 1.0f, 1.0f, 1.37f, 0.5f };
	static const float gain[6] = { 0.8f, 0.5f, 0.3f, 0.6f, 0.4f, 0.7f };
	static const int which[6] = { 0, 1, 2, 3, 0, 2 };
	for (i = 0; i < 6; i++) {
		alSourcei(src[i], AL_BUFFER, (ALint)buf[which[i]]);
		alSourcef(src[i], AL_PITCH, pitch[i]);
		alSourcef(src[i], AL_GAIN, gain[i]);
		alSourcei(src[i], AL_LOOPING, AL_TRUE);
		alSource3f(src[i], AL_POSITION, (float)(i - 2.5), 0.0f, -1.0f);
	}
	alSourcePlayv(6, src);
	for (s = 0; s < seconds; s++) {
		uint64_t h = 0xcbf29ce484222325ull;
		int left = RATE;

		/* every second: positions, pitch and gain move, a source stops or starts */
		for (i = 0; i < 6; i++) {
			alSource3f(src[i], AL_POSITION, (float)sin(s + i), 0.0f, (float)-cos(s * 0.7 + i));
			alSourcef(src[i], AL_PITCH, pitch[i] * (1.0f + 0.05f * (float)((s + i) % 4)));
		}
		alSourcef(src[0], AL_GAIN, 0.2f + 0.1f * (float)(s % 5));
		if (s == 2)
			alSourceStop(src[3]);
		if (s == 4)
			alSourcePlay(src[3]);
		while (left > 0) {
			int n = left < 1024 ? left : 1024;

			render(dev, out, n);
			h = fnv(h, out, (size_t)n * 4);
			all = fnv(all, out, (size_t)n * 4);
			left -= n;
		}
		printf("second %d: %016llx\n", s, (unsigned long long)h);
	}
	printf("all %d s: %016llx (al error %d)\n", seconds, (unsigned long long)all, alGetError());
	alcMakeContextCurrent(NULL);
	alcDestroyContext(ctx);
	alcCloseDevice(dev);
	return 0;
}
