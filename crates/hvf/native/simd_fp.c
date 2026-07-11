#include <Hypervisor/Hypervisor.h>
#include <stdint.h>
#include <string.h>

_Static_assert(sizeof(hv_simd_fp_uchar16_t) == 16,
               "Hypervisor.framework SIMD/FP values must remain 16 bytes");

hv_return_t bangbang_hv_vcpu_set_simd_fp_reg(hv_vcpu_t vcpu,
                                              hv_simd_fp_reg_t reg,
                                              const uint8_t value[static 16]) {
    hv_simd_fp_uchar16_t simd_value;
    memcpy(&simd_value, value, sizeof(simd_value));
    return hv_vcpu_set_simd_fp_reg(vcpu, reg, simd_value);
}
