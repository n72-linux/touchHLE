@ minimal iOS 4 entry (LC_UNIXTHREAD): sp -> argc, argv[], 0, envp[]
	.text
	.globl start
	.align 2
start:
	ldr	r0, [sp]		@ argc
	add	r1, sp, #4		@ argv
	add	r2, r1, r0, lsl #2
	add	r2, r2, #4		@ envp
	bic	sp, sp, #7		@ 8-byte align
	bl	_main
	bl	_exit
1:	b	1b
