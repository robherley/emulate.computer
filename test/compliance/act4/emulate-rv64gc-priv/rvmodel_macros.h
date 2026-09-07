# rvmodel_macros.h
# DUT-specific macro definitions for emulate.computer and its Sail reference model
# SPDX-License-Identifier: BSD-3-Clause

#ifndef _RVMODEL_MACROS_H
#define _RVMODEL_MACROS_H

#define CLINT_BASE_ADDRESS 0x02000000

#define RVMODEL_DATA_SECTION \
        .pushsection .tohost,"aw",@progbits;                \
        .balign 8; .global tohost; tohost: .dword 0;         \
        .balign 8; .global fromhost; fromhost: .dword 0;     \
        .popsection

#define STANDARD_SM_SUPPORTED

##### STARTUP #####

# Perform boot operations. Can be empty or left undefined unless needed for
# DUT-specific behavior such as turning on a memory controller or
# initializing custom state.
// #define RVMODEL_BOOT

// Custom RVMODEL_BOOT_TO_MMODE overrides default RVTEST_BOOT_TO_MMODE
// if defined.  For most DUTs, the default should work and this macro
// should not be defined.  If no M-mode or CSRs are implemented, define this
// macro as blank to bypass the boot process.  If a nonconforming
// M-mode is implemented, define this macro to set up the necessary
// state in a fashion similar to RVTEST_BOOT_TO_MMODE.
//#define RVMODEL_BOOT_TO_MMODE

##### TERMINATION #####

// Both Sail and emulate monitor tohost. Value 1 is pass; another odd value
// is failure.

# Terminate test with a pass indication.
# When the test is run in simulation, this should end the simulation.
#define RVMODEL_HALT_PASS  \
  li x1, 1                ;\
  la t0, tohost           ;\
  write_tohost_pass:      ;\
    sw x1, 0(t0)          ;\
    sw x0, 4(t0)          ;\
    j write_tohost_pass   ;\


# Terminate test with a fail indication.
# When the test is run in simulation, this should end the simulation.
#define RVMODEL_HALT_FAIL \
  li x1, 3                ;\
  la t0, tohost           ;\
  write_tohost_fail:      ;\
    sw x1, 0(t0)          ;\
    sw x0, 4(t0)          ;\
    j write_tohost_fail   ;\


##### IO #####

.EQU UART_BASE_ADDR, 0x10000000
.EQU UART_THR, (UART_BASE_ADDR + 0)
.EQU UART_LCR, (UART_BASE_ADDR + 3)
.EQU UART_LSR, (UART_BASE_ADDR + 5)

# Initialization steps needed prior to writing to the console
# _R1, _R2, and _R3 can be used as temporary registers if needed.
# Do not modify any other registers (or make sure to restore them).
# Can be empty or left undefined if no initialization is needed.
#define RVMODEL_IO_INIT(_R1, _R2, _R3) \
  li _R1, UART_LCR;                    \
  li _R2, 3;                           \
  sb _R2, 0(_R1);


# Prints a null-terminated string using a DUT specific mechanism.
# A pointer to the string is passed in _STR_PTR.
# _R1, _R2, and _R3 can be used as temporary registers if needed.
# Do not modify any other registers (or make sure to restore them).
#define RVMODEL_IO_WRITE_STR(_R1, _R2, _R3, _STR_PTR) \
1:                                                     \
  lbu _R1, 0(_STR_PTR);                                \
  beqz _R1, 3f;                                        \
  li _R2, UART_LSR;                                    \
2:                                                     \
  lbu _R3, 0(_R2);                                     \
  andi _R3, _R3, 0x20;                                 \
  beqz _R3, 2b;                                        \
  li _R2, UART_THR;                                    \
  sb _R1, 0(_R2);                                      \
  addi _STR_PTR, _STR_PTR, 1;                          \
  j 1b;                                                \
3:

##### Access Fault #####

#define RVMODEL_ACCESS_FAULT_ADDRESS 0x00000000

##### Machine Timer #####

#define RVMODEL_MTIMECMP_ADDRESS  0x02004000  /* Address of mtimecmp CSR */

#define RVMODEL_MTIME_ADDRESS  0x0200BFF8  /* Address of mtime CSR */

##### Machine Interrupts #####
#define RVMODEL_MAX_CYCLES_PER_TIMER_TICK 10

// Interrupt latency configuration
#define RVMODEL_INTERRUPT_LATENCY 10

#define RVMODEL_TIMER_INT_SOON_DELAY 100

#define RVMODEL_SIMPLE_IRQ_COMMAND 0x10010004
#define RVMODEL_SET_MEXT_INT(_R1, _R2) \
  li _R1, (1 << 31) | (1 << 11);       \
  li _R2, RVMODEL_SIMPLE_IRQ_COMMAND;  \
  sw _R1, 0(_R2);
#define RVMODEL_CLR_MEXT_INT(_R1, _R2) \
  li _R1, (1 << 11);                   \
  li _R2, RVMODEL_SIMPLE_IRQ_COMMAND;  \
  sw _R1, 0(_R2);

#define RVMODEL_MSIP_ADDRESS (CLINT_BASE_ADDRESS + 0x0)
#define RVMODEL_SET_MSW_INT(_R1, _R2)        \
  li _R1, 1;                 \
  li _R2, RVMODEL_MSIP_ADDRESS;              \
  sw _R1, 0(_R2);


#define RVMODEL_CLR_MSW_INT(_R1, _R2)        \
  li _R2, RVMODEL_MSIP_ADDRESS;              \
  sw zero, 0(_R2);



##### Supervisor Interrupts #####

#define RVMODEL_SET_SEXT_INT(_R1, _R2) \
  li _R1, (1 << 31) | (1 << 9);        \
  li _R2, RVMODEL_SIMPLE_IRQ_COMMAND;  \
  sw _R1, 0(_R2);
#define RVMODEL_CLR_SEXT_INT(_R1, _R2) \
  li _R1, (1 << 9);                    \
  li _R2, RVMODEL_SIMPLE_IRQ_COMMAND;  \
  sw _R1, 0(_R2);
#define RVMODEL_SET_SSW_INT(_R1, _R2)  \
  li _R1, (1 << 31) | (1 << 1);        \
  li _R2, RVMODEL_SIMPLE_IRQ_COMMAND;  \
  sw _R1, 0(_R2);
#define RVMODEL_CLR_SSW_INT(_R1, _R2)  \
  li _R1, (1 << 1);                    \
  li _R2, RVMODEL_SIMPLE_IRQ_COMMAND;  \
  sw _R1, 0(_R2);

#endif // _RVMODEL_MACROS_H
