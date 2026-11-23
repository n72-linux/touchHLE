/*
 * Hand-written declarations for N72MediaTest: there is no iOS SDK in the
 * ios-armv6-tc image, so only what the app uses is declared here. Each
 * class/function must also be exported by the matching stub in stubs/ and be
 * implemented by touchHLE (see README.md).
 *
 * iPhone OS 3 armv6 ABI: CGFloat is float, BOOL is signed char, NSInteger
 * is int; structs larger than 4 bytes (CGRect) come back through
 * objc_msgSend_stret (clang picks it from the declared return type).
 */
#ifndef N72_MEDIATEST_DECLS_H
#define N72_MEDIATEST_DECLS_H

typedef signed char BOOL;
#define YES ((BOOL)1)
#define NO ((BOOL)0)
#define nil ((id)0)
#define NULL ((void *)0)
typedef int NSInteger;
typedef unsigned int NSUInteger;
typedef double NSTimeInterval;
typedef double CFAbsoluteTime;
typedef float CGFloat;
typedef struct { CGFloat x, y; } CGPoint;
typedef struct { CGFloat width, height; } CGSize;
typedef struct { CGPoint origin; CGSize size; } CGRect;

/* libSystem */
typedef struct __sFILE FILE;
FILE *fopen(const char *path, const char *mode);
char *fgets(char *buf, int size, FILE *f);
int fclose(FILE *f);
unsigned long strlen(const char *s);
int strcmp(const char *a, const char *b);

/* CoreFoundation */
CFAbsoluteTime CFAbsoluteTimeGetCurrent(void);

@class NSString, NSURL, NSNotification, NSTimer, UIColor, UIApplication;

/* Foundation */
__attribute__((objc_root_class))
@interface NSObject {
  id isa;
}
+ (id)alloc;
+ (id)new;
- (id)init;
- (id)retain;
- (void)release;
- (id)autorelease;
@end

@interface NSAutoreleasePool : NSObject
@end

@interface NSString : NSObject
+ (id)stringWithUTF8String:(const char *)s;
- (const char *)UTF8String;
- (NSString *)stringByAppendingPathComponent:(NSString *)c;
@end

@interface NSBundle : NSObject
+ (NSBundle *)mainBundle;
- (NSString *)pathForResource:(NSString *)name ofType:(NSString *)ext;
- (NSString *)bundlePath;
@end

@interface NSFileManager : NSObject
+ (NSFileManager *)defaultManager;
- (BOOL)fileExistsAtPath:(NSString *)path;
@end

@interface NSURL : NSObject
+ (NSURL *)fileURLWithPath:(NSString *)path;
@end

@interface NSNotificationCenter : NSObject
+ (NSNotificationCenter *)defaultCenter;
- (void)addObserver:(id)observer
           selector:(SEL)sel
               name:(NSString *)name
             object:(id)object;
@end

@interface NSTimer : NSObject
+ (NSTimer *)scheduledTimerWithTimeInterval:(NSTimeInterval)interval
                                     target:(id)target
                                   selector:(SEL)sel
                                   userInfo:(id)info
                                    repeats:(BOOL)repeats;
- (void)invalidate;
@end

@interface NSError : NSObject
- (NSInteger)code;
@end

void NSLog(NSString *format, ...);

/* UIKit */
int UIApplicationMain(int argc, char **argv, NSString *principal,
                      NSString *delegate);

@interface UIScreen : NSObject
+ (UIScreen *)mainScreen;
- (CGRect)bounds;
@end

@interface UIColor : NSObject
+ (UIColor *)greenColor;
+ (UIColor *)blueColor;
+ (UIColor *)redColor;
@end

@interface UIView : NSObject
- (id)initWithFrame:(CGRect)frame;
- (void)setBackgroundColor:(UIColor *)color;
@end

@interface UIWindow : UIView
- (void)makeKeyAndVisible;
@end

/* MediaPlayer */
extern NSString *const MPMoviePlayerPlaybackDidFinishNotification;

@interface MPMoviePlayerController : NSObject
- (id)initWithContentURL:(NSURL *)url;
- (void)play;
@end

/* AVFoundation */
@interface AVAudioPlayer : NSObject
- (id)initWithContentsOfURL:(NSURL *)url error:(NSError **)error;
- (void)setDelegate:(id)delegate;
- (void)setNumberOfLoops:(NSInteger)loops;
- (NSTimeInterval)duration;
- (BOOL)isPlaying;
- (BOOL)play;
- (void)stop;
@end

#endif
