// Copyright (c) 2019 Stefan Lankes, RWTH Aachen University
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

#![allow(dead_code)]

extern "C" {
	fn loader_main();
}

const BOOT_STACK_SIZE: usize = 4096;

#[link_section = ".data"]
static STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

/*
 * Memory types available.
 */
#[allow(non_upper_case_globals)]
const MT_DEVICE_nGnRnE: u64 = 0;
#[allow(non_upper_case_globals)]
const MT_DEVICE_nGnRE: u64 = 1;
const MT_DEVICE_GRE: u64 = 2;
const MT_NORMAL_NC: u64 = 3;
const MT_NORMAL: u64 = 4;

fn mair(attr: u64, mt: u64) -> u64 {
	attr << (mt * 8)
}

/*
 * TCR flags
 */
const TCR_IRGN_WBWA: u64 = ((1) << 8) | ((1) << 24);
const TCR_ORGN_WBWA: u64 = ((1) << 10) | ((1) << 26);
const TCR_SHARED: u64 = ((3) << 12) | ((3) << 28);
const TCR_TBI0: u64 = 1 << 37;
const TCR_TBI1: u64 = 1 << 38;
const TCR_ASID16: u64 = 1 << 36;
const TCR_TG1_16K: u64 = 1 << 30;
const TCR_TG1_4K: u64 = 0 << 30;
const TCR_FLAGS: u64 = TCR_IRGN_WBWA | TCR_ORGN_WBWA | TCR_SHARED;

/// Number of virtual address bits for 4KB page
const VA_BITS: u64 = 48;

fn tcr_size(x: u64) -> u64 {
	(((64) - (x)) << 16) | (((64) - (x)) << 0)
}

#[inline(never)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn _start() -> ! {
	// Pointer to stack base
	llvm_asm!("mov sp, $0"
		:: "r"(&STACK[BOOT_STACK_SIZE - 0x10] as *const u8 as usize)
        :: "volatile");

	pre_init();
}

unsafe fn pre_init() -> ! {
	loaderlog!("Enter startup code");

	/* disable interrupts */
	llvm_asm!("msr daifset, #0b111" :::: "volatile");

	/* reset thread id registers */
	llvm_asm!("msr tpidr_el0, $0\n\t
        msr tpidr_el1, $0" :: "r"(0) :: "volatile");

	/*
	 * Disable the MMU. We may have entered the kernel with it on and
	 * will need to update the tables later. If this has been set up
	 * with anything other than a VA == PA map then this will fail,
	 * but in this case the code to find where we are running from
	 * would have also failed.
	 */
	llvm_asm!("dsb sy\n\t
        mrs x2, sctlr_el1\n\t
        bic x2, x2, #0x1\n\t
        msr sctlr_el1, x2\n\t
        isb" ::: "x2" : "volatile");

	llvm_asm!("ic iallu\n\t
        tlbi vmalle1is\n\t
        dsb ish" :::: "volatile");

	/*
	 * Setup memory attribute type tables
	 *
	 * Memory regioin attributes for LPAE:
	 *
	 *   n = AttrIndx[2:0]
	 *                      n       MAIR
	 *   DEVICE_nGnRnE      000     00000000 (0x00)
	 *   DEVICE_nGnRE       001     00000100 (0x04)
	 *   DEVICE_GRE         010     00001100 (0x0c)
	 *   NORMAL_NC          011     01000100 (0x44)
	 *   NORMAL             100     11111111 (0xff)
	 */
	let mair_el1 = mair(0x00, MT_DEVICE_nGnRnE)
		| mair(0x04, MT_DEVICE_nGnRE)
		| mair(0x0c, MT_DEVICE_GRE)
		| mair(0x44, MT_NORMAL_NC)
		| mair(0xff, MT_NORMAL);
	llvm_asm!("msr mair_el1, $0" :: "r"(mair_el1) :: "volatile");

	/*
	 * Setup translation control register (TCR)
	 */

	// determine physical address size
	llvm_asm!("mrs x0, id_aa64mmfr0_el1\n\t
        and x0, x0, 0xF\n\t
        lsl x0, x0, 32\n\t
        orr x0, x0, $0\n\t
        mrs x1, id_aa64mmfr0_el1\n\t
        bfi x0, x1, #32, #3\n\t
        msr tcr_el1, x0"
        :: "r"(tcr_size(VA_BITS) | TCR_TG1_4K | TCR_FLAGS)
        : "x0", "x1" : "volatile");

	/*
	 * Enable FP/ASIMD in Architectural Feature Access Control Register,
	 */
	llvm_asm!("msr cpacr_el1, $0" :: "r"(3 << 20) :: "volatile");

	/*
	 * Reset debug controll register
	 */
	llvm_asm!("msr mdscr_el1, xzr" :::: "volatile");

	/* Turning on MMU */
	llvm_asm!("dsb sy" :::: "volatile");

	/*
	* Prepare system control register (SCTRL)
	*
	*
	*   UCI     [26] Enables EL0 access in AArch64 for DC CVAU, DC CIVAC,
					 DC CVAC and IC IVAU instructions
	*   EE      [25] Explicit data accesses at EL1 and Stage 1 translation
					 table walks at EL1 & EL0 are little-endian
	*   EOE     [24] Explicit data accesses at EL0 are little-endian
	*   WXN     [19] Regions with write permission are not forced to XN
	*   nTWE    [18] WFE instructions are executed as normal
	*   nTWI    [16] WFI instructions are executed as normal
	*   UCT     [15] Enables EL0 access in AArch64 to the CTR_EL0 register
	*   DZE     [14] Execution of the DC ZVA instruction is allowed at EL0
	*   I       [12] Instruction caches enabled at EL0 and EL1
	*   UMA     [9]  Disable access to the interrupt masks from EL0
	*   SED     [8]  The SETEND instruction is available
	*   ITD     [7]  The IT instruction functionality is available
	*   THEE    [6]  ThumbEE is disabled
	*   CP15BEN [5]  CP15 barrier operations disabled
	*   SA0     [4]  Stack Alignment check for EL0 enabled
	*   SA      [3]  Stack Alignment check enabled
	*   C       [2]  Data and unified enabled
	*   A       [1]  Alignment fault checking disabled
	*   M       [0]  MMU enable
	*/
	llvm_asm!("msr sctlr_el1, $0" :: "r"(0x4D5D91C) :: "volatile");

	// Enter loader
	loader_main();

	// we should never reach this  point
	loop {}
}
