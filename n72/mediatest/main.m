/*
 * N72MediaTest: a tiny iPhone OS 3 app for testing touchHLE's media
 * support (n72 fork) without navigating a game.
 *
 * It shows a green window, then runs the steps in the bundle file
 * test.txt one after another, 3 s apart, and logs every step with NSLog
 * (prefix "N72MediaTest:"). Step format, one per line:
 *
 *   movie FILE            MPMoviePlayerController on the bundle file FILE
 *   audio FILE [SECONDS]  AVAudioPlayer on FILE, numberOfLoops 1, stopped
 *                         after SECONDS (default 8) or when it finishes
 *   # comment             (and empty lines) ignored
 *
 * New actions go into runNextStep:.
 * Window colours: green = before the first step, blue = a step finished,
 * red = a step failed (file missing, unknown action).
 *
 * Style (like touchHLE's TestApp): no properties, blocks, ARC or object
 * literals; state in file-scope statics.
 */
#include "decls.h"

#define MAX_STEPS 32
#define MAX_LINE 256
#define STEP_GAP 3.0 /* seconds between steps */

static char step_action[MAX_STEPS][16];
static char step_file[MAX_STEPS][MAX_LINE];
static int step_count;
static int step_next;

static UIWindow *window;
static MPMoviePlayerController *player;
static CFAbsoluteTime play_start;

#define AUDIO_DEFAULT_SECONDS 8.0
#define AUDIO_POLL 0.25 /* seconds */

static AVAudioPlayer *audio;
static NSTimer *audio_poll;
static double audio_limit;
static BOOL audio_was_playing;

/* "FILE [SECONDS]": split off a trailing number, if there is one. */
static double take_seconds(char *file, double dflt) {
  char *p = file + strlen(file), *num;
  double v = 0, scale = 0;

  while (p > file && p[-1] != ' ' && p[-1] != '\t')
    p--;
  if (p == file || *p == 0)
    return dflt;
  for (num = p; *num; num++) {
    if (*num == '.' && scale == 0)
      scale = 1;
    else if (*num >= '0' && *num <= '9') {
      v = v * 10 + (*num - '0');
      if (scale != 0)
        scale *= 10;
    } else
      return dflt; /* not a number: part of the file name */
  }
  if (scale != 0)
    v /= scale;
  while (p > file && (p[-1] == ' ' || p[-1] == '\t'))
    p--;
  *p = 0;
  return v > 0 ? v : dflt;
}

/* Split "action file" into the step table; false on a bad line. */
static BOOL add_step(char *line) {
  char *p = line, *action, *file;
  unsigned long n;

  /* strip the line end and leading/trailing blanks */
  n = strlen(p);
  while (n > 0 && (p[n - 1] == '\n' || p[n - 1] == '\r' || p[n - 1] == ' ' ||
                   p[n - 1] == '\t'))
    p[--n] = 0;
  while (*p == ' ' || *p == '\t')
    p++;
  if (*p == 0 || *p == '#')
    return YES;

  action = p;
  while (*p && *p != ' ' && *p != '\t')
    p++;
  if (*p)
    *p++ = 0;
  while (*p == ' ' || *p == '\t')
    p++;
  file = p; /* the rest of the line: may contain spaces */

  if (step_count == MAX_STEPS || strlen(action) >= sizeof step_action[0])
    return NO;
  n = strlen(action);
  for (unsigned long i = 0; i <= n; i++)
    step_action[step_count][i] = action[i];
  n = strlen(file);
  for (unsigned long i = 0; i <= n; i++)
    step_file[step_count][i] = file[i];
  step_count++;
  return YES;
}

static void load_steps(void) {
  NSString *path =
      [[NSBundle mainBundle] pathForResource:@"test" ofType:@"txt"];
  FILE *f;
  char line[MAX_LINE];

  if (path == nil) {
    NSLog(@"N72MediaTest: no test.txt in the bundle");
    return;
  }
  f = fopen([path UTF8String], "r");
  if (f == NULL) {
    NSLog(@"N72MediaTest: cannot open %@", path);
    return;
  }
  while (fgets(line, sizeof line, f) != NULL) {
    if (!add_step(line))
      NSLog(@"N72MediaTest: bad line skipped: %s", line);
  }
  fclose(f);
  NSLog(@"N72MediaTest: %d step(s) in test.txt", step_count);
}

/* Bundle path of a file name, nil if it is not in the bundle. */
static NSString *bundle_file(const char *name) {
  NSString *path = [[[NSBundle mainBundle] bundlePath]
      stringByAppendingPathComponent:[NSString stringWithUTF8String:name]];
  if (![[NSFileManager defaultManager] fileExistsAtPath:path])
    return nil;
  return path;
}

@interface N72MediaTestDelegate : NSObject
- (void)applicationDidFinishLaunching:(UIApplication *)app;
- (void)runNextStep:(NSTimer *)timer;
- (void)stepDone:(BOOL)ok;
- (void)movieFinished:(NSNotification *)note;
- (void)audioPoll:(NSTimer *)timer;
- (void)audioEnd:(const char *)why;
- (void)audioPlayerDidFinishPlaying:(AVAudioPlayer *)p successfully:(BOOL)ok;
@end

@implementation N72MediaTestDelegate

- (void)applicationDidFinishLaunching:(UIApplication *)app {
  NSLog(@"N72MediaTest: launched");
  window = [[UIWindow alloc] initWithFrame:[[UIScreen mainScreen] bounds]];
  [window setBackgroundColor:[UIColor greenColor]];
  [window makeKeyAndVisible];

  /* once, not per movie step: a second registration would deliver each
     notification twice */
  [[NSNotificationCenter defaultCenter]
      addObserver:self
         selector:@selector(movieFinished:)
             name:MPMoviePlayerPlaybackDidFinishNotification
           object:nil];

  load_steps();
  /* first step after a short pause, so the green frame is seen */
  [NSTimer scheduledTimerWithTimeInterval:STEP_GAP
                                   target:self
                                 selector:@selector(runNextStep:)
                                 userInfo:nil
                                  repeats:NO];
}

- (void)runNextStep:(NSTimer *)timer {
  int i;
  NSString *path;

  if (step_next >= step_count) {
    NSLog(@"N72MediaTest: done");
    return; /* keep running */
  }
  i = step_next++;

  if (!strcmp(step_action[i], "movie")) {
    NSLog(@"N72MediaTest: movie %s start", step_file[i]);
    path = bundle_file(step_file[i]);
    if (path == nil) {
      NSLog(@"N72MediaTest: movie %s: not in the bundle", step_file[i]);
      [self stepDone:NO];
      return;
    }
    player = [[MPMoviePlayerController alloc]
        initWithContentURL:[NSURL fileURLWithPath:path]];
    play_start = CFAbsoluteTimeGetCurrent();
    [player play];
    return; /* movieFinished: goes on */
  }

  if (!strcmp(step_action[i], "audio")) {
    NSError *err = nil;
    char *file = step_file[i];

    audio_limit = take_seconds(file, AUDIO_DEFAULT_SECONDS);
    NSLog(@"N72MediaTest: audio %s start", file);
    path = bundle_file(file);
    if (path == nil) {
      NSLog(@"N72MediaTest: audio %s: not in the bundle", file);
      [self stepDone:NO];
      return;
    }
    audio = [[AVAudioPlayer alloc] initWithContentsOfURL:[NSURL fileURLWithPath:path]
                                                   error:&err];
    if (audio == nil) {
      NSLog(@"N72MediaTest: audio %s: cannot open, error code %d", file,
            err != nil ? [err code] : 0);
      [self stepDone:NO];
      return;
    }
    [audio setDelegate:self];
    [audio setNumberOfLoops:1];
    NSLog(@"N72MediaTest: audio %s duration %.3f s, loops 1, limit %.1f s",
          file, [audio duration], audio_limit);
    play_start = CFAbsoluteTimeGetCurrent();
    if (![audio play])
      NSLog(@"N72MediaTest: audio %s: play returned NO", file);
    audio_was_playing = YES;
    audio_poll =
        [NSTimer scheduledTimerWithTimeInterval:AUDIO_POLL
                                         target:self
                                       selector:@selector(audioPoll:)
                                       userInfo:nil
                                        repeats:YES];
    return; /* audioPoll: or the delegate goes on */
  }

  NSLog(@"N72MediaTest: unknown action %s skipped", step_action[i]);
  [self stepDone:NO];
}

- (void)stepDone:(BOOL)ok {
  [window setBackgroundColor:ok ? [UIColor blueColor] : [UIColor redColor]];
  [NSTimer scheduledTimerWithTimeInterval:STEP_GAP
                                   target:self
                                 selector:@selector(runNextStep:)
                                 userInfo:nil
                                  repeats:NO];
}

- (void)movieFinished:(NSNotification *)note {
  CFAbsoluteTime elapsed = CFAbsoluteTimeGetCurrent() - play_start;
  if (player == nil) {
    NSLog(@"N72MediaTest: movie finished, but no player of ours");
    return;
  }
  NSLog(@"N72MediaTest: movie finished after %.2f s", elapsed);
  /* no stop: touchHLE drops its own reference once it has posted this */
  [player release];
  player = nil;
  [self stepDone:YES];
}

/* Every AUDIO_POLL s: the time limit, and isPlaying for the log. */
- (void)audioPoll:(NSTimer *)timer {
  CFAbsoluteTime elapsed = CFAbsoluteTimeGetCurrent() - play_start;
  if (audio == nil)
    return;
  if (audio_was_playing && ![audio isPlaying]) {
    /* the last packets are queued, not yet heard: keep going */
    audio_was_playing = NO;
    NSLog(@"N72MediaTest: audio isPlaying NO after %.2f s", elapsed);
  }
  if (elapsed >= audio_limit)
    [self audioEnd:"stopped at the time limit"];
}

- (void)audioPlayerDidFinishPlaying:(AVAudioPlayer *)p successfully:(BOOL)ok {
  if (p != audio || audio == nil) {
    NSLog(@"N72MediaTest: audioPlayerDidFinishPlaying for no player of ours");
    return;
  }
  [self audioEnd:ok ? "finished (delegate, successfully YES)"
                    : "finished (delegate, successfully NO)"];
}

- (void)audioEnd:(const char *)why {
  CFAbsoluteTime elapsed = CFAbsoluteTimeGetCurrent() - play_start;
  NSLog(@"N72MediaTest: audio %s after %.2f s", why, elapsed);
  [audio_poll invalidate]; /* the run loop's reference goes */
  audio_poll = nil;
  [audio setDelegate:nil];
  [audio stop];
  [audio release];
  audio = nil;
  [self stepDone:YES];
}

@end

int main(int argc, char **argv) {
  NSAutoreleasePool *pool = [[NSAutoreleasePool alloc] init];
  int ret = UIApplicationMain(argc, argv, nil, @"N72MediaTestDelegate");
  [pool release];
  return ret;
}
