#include <stdint.h>

uint8_t bangbang_macos_15_2_system_registers_available(void) {
    if (__builtin_available(macOS 15.2, *)) {
        return 1;
    }
    return 0;
}
