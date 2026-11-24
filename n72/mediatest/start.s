@ minimal iOS entry (LC_UNIXTHREAD): sp -> argc, argv[], 0, envp[]
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

@ An iPhone OS 3.0 link (classic relocations, no LC_DYLD_INFO) wants
@ crt1.o's dyld_stub_binding_helper: the lazy pointers start out pointing
@ at it. touchHLE never runs it: it rewrites every stub at load to call
@ its own linker (Dyld::setup_lazy_linking). If anything does jump here,
@ trap instead of running on.
	.private_extern dyld_stub_binding_helper
	.align 2
dyld_stub_binding_helper:
	trap
