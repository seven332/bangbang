#include "bangbang_mach_exc_server.h"
#include "bangbang_mach_exc_user.h"

#include <mach/arm/thread_status.h>
#include <mach/exc.h>
#include <mach/exception_types.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <mach/vm_map.h>

#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum {
    BANGBANG_MACH_OK = 0,
    BANGBANG_MACH_INVALID = 1,
    BANGBANG_MACH_NO_MEMORY = 2,
    BANGBANG_MACH_OPERATION_FAILED = 3,
    BANGBANG_MACH_OWNER_BUSY = 4,
    BANGBANG_MACH_RESTORE_FAILED = 5,
    BANGBANG_MACH_THREAD_FAILED = 6,
    BANGBANG_MACH_FAULT_FORWARD = 0,
    BANGBANG_MACH_FAULT_HANDLED = 1,
    BANGBANG_MACH_FAULT_TERMINAL = 2,
    BANGBANG_MACH_ACCESS_READ = 1,
    BANGBANG_MACH_ACCESS_WRITE = 2,
    BANGBANG_MACH_TERMINAL_EXIT_CODE = 70,
    BANGBANG_MACH_SERVER_MESSAGE_SIZE = 32768,
    BANGBANG_MACH_STOP_MESSAGE_ID = 0x42424d46,
};

struct bangbang_mach_region {
    void *primary;
    void *alias;
    size_t size;
};

struct bangbang_mach_lazy_mapping {
    struct bangbang_mach_region *regions;
    size_t region_count;
    size_t page_size;
};

typedef uint32_t (*bangbang_mach_fault_callback)(
    void *context,
    uint64_t address,
    uint32_t access);

struct bangbang_mach_exception_owner {
    mach_port_t port;
    pthread_t thread;
    bool thread_started;
    bool installed;
    _Atomic bool stop;
    void *context;
    bangbang_mach_fault_callback callback;
    exception_mask_t previous_masks[EXC_TYPES_COUNT];
    mach_port_t previous_ports[EXC_TYPES_COUNT];
    exception_behavior_t previous_behaviors[EXC_TYPES_COUNT];
    thread_state_flavor_t previous_flavors[EXC_TYPES_COUNT];
    mach_msg_type_number_t previous_count;
};

struct bangbang_mach_region_input {
    void *primary;
    size_t size;
};

static _Atomic(struct bangbang_mach_exception_owner *) current_owner;
static pthread_mutex_t owner_install_lock = PTHREAD_MUTEX_INITIALIZER;

static int mapping_page_size(size_t *page_size) {
    long value = sysconf(_SC_PAGESIZE);
    if (value <= 0) {
        return BANGBANG_MACH_OPERATION_FAILED;
    }
    *page_size = (size_t)value;
    return BANGBANG_MACH_OK;
}

static bool checked_host_range(void *address, size_t size, uintptr_t *end) {
    uintptr_t start = (uintptr_t)address;
    if (start == 0 || size == 0 || start > UINTPTR_MAX - size) {
        return false;
    }
    *end = start + size;
    return true;
}

static bool valid_region_inputs(
    const struct bangbang_mach_region_input *inputs,
    size_t count,
    size_t page_size) {
    if (inputs == NULL || count == 0 ||
        count > SIZE_MAX / sizeof(struct bangbang_mach_region)) {
        return false;
    }
    for (size_t index = 0; index < count; ++index) {
        uintptr_t end = 0;
        uintptr_t start = (uintptr_t)inputs[index].primary;
        if (!checked_host_range(inputs[index].primary, inputs[index].size, &end) ||
            start % page_size != 0 || inputs[index].size % page_size != 0) {
            return false;
        }
        for (size_t previous = 0; previous < index; ++previous) {
            uintptr_t previous_end = 0;
            uintptr_t previous_start = (uintptr_t)inputs[previous].primary;
            if (!checked_host_range(
                    inputs[previous].primary,
                    inputs[previous].size,
                    &previous_end) ||
                (start < previous_end && previous_start < end)) {
                return false;
            }
        }
    }
    return true;
}

static void destroy_region_alias(struct bangbang_mach_region *region) {
    if (region->alias != NULL && region->size != 0) {
        (void)mach_vm_deallocate(
            mach_task_self(),
            (mach_vm_address_t)(uintptr_t)region->alias,
            (mach_vm_size_t)region->size);
        region->alias = NULL;
    }
}

static int create_region_alias(
    struct bangbang_mach_region *region,
    const struct bangbang_mach_region_input *input) {
    memory_object_size_t entry_size = (memory_object_size_t)input->size;
    mach_port_t entry = MACH_PORT_NULL;
    kern_return_t result = mach_make_memory_entry_64(
        mach_task_self(),
        &entry_size,
        (memory_object_offset_t)(uintptr_t)input->primary,
        VM_PROT_READ | VM_PROT_WRITE,
        &entry,
        MACH_PORT_NULL);
    if (result != KERN_SUCCESS || entry == MACH_PORT_NULL ||
        entry_size != (memory_object_size_t)input->size) {
        if (entry != MACH_PORT_NULL) {
            (void)mach_port_deallocate(mach_task_self(), entry);
        }
        return BANGBANG_MACH_OPERATION_FAILED;
    }

    mach_vm_address_t alias = 0;
    result = mach_vm_map(
        mach_task_self(),
        &alias,
        (mach_vm_size_t)input->size,
        0,
        VM_FLAGS_ANYWHERE,
        entry,
        0,
        FALSE,
        VM_PROT_READ | VM_PROT_WRITE,
        VM_PROT_READ | VM_PROT_WRITE,
        VM_INHERIT_NONE);
    (void)mach_port_deallocate(mach_task_self(), entry);
    if (result != KERN_SUCCESS || alias == 0) {
        return BANGBANG_MACH_OPERATION_FAILED;
    }

    region->primary = input->primary;
    region->alias = (void *)(uintptr_t)alias;
    region->size = input->size;
    return BANGBANG_MACH_OK;
}

int bangbang_mach_lazy_mapping_create(
    const struct bangbang_mach_region_input *inputs,
    size_t count,
    struct bangbang_mach_lazy_mapping **output) {
    if (output == NULL) {
        return BANGBANG_MACH_INVALID;
    }
    *output = NULL;

    size_t page_size = 0;
    int status = mapping_page_size(&page_size);
    if (status != BANGBANG_MACH_OK ||
        !valid_region_inputs(inputs, count, page_size)) {
        return BANGBANG_MACH_INVALID;
    }

    struct bangbang_mach_lazy_mapping *mapping =
        calloc(1, sizeof(*mapping));
    if (mapping == NULL) {
        return BANGBANG_MACH_NO_MEMORY;
    }
    mapping->regions = calloc(count, sizeof(*mapping->regions));
    if (mapping->regions == NULL) {
        free(mapping);
        return BANGBANG_MACH_NO_MEMORY;
    }
    mapping->region_count = count;
    mapping->page_size = page_size;

    for (size_t index = 0; index < count; ++index) {
        status = create_region_alias(&mapping->regions[index], &inputs[index]);
        if (status != BANGBANG_MACH_OK) {
            for (size_t prior = 0; prior < index; ++prior) {
                destroy_region_alias(&mapping->regions[prior]);
            }
            free(mapping->regions);
            free(mapping);
            return status;
        }
    }

    *output = mapping;
    return BANGBANG_MACH_OK;
}

static int protect_region(
    const struct bangbang_mach_region *region,
    size_t offset,
    size_t length,
    vm_prot_t protection,
    size_t page_size) {
    if (region == NULL || offset > region->size ||
        length == 0 || length > region->size - offset ||
        offset % page_size != 0 || length % page_size != 0) {
        return BANGBANG_MACH_INVALID;
    }
    uintptr_t start = (uintptr_t)region->primary;
    if (start > UINTPTR_MAX - offset) {
        return BANGBANG_MACH_INVALID;
    }
    kern_return_t result = mach_vm_protect(
        mach_task_self(),
        (mach_vm_address_t)(start + offset),
        (mach_vm_size_t)length,
        FALSE,
        protection);
    return result == KERN_SUCCESS
        ? BANGBANG_MACH_OK
        : BANGBANG_MACH_OPERATION_FAILED;
}

int bangbang_mach_lazy_mapping_protect_all_none(
    struct bangbang_mach_lazy_mapping *mapping) {
    if (mapping == NULL) {
        return BANGBANG_MACH_INVALID;
    }
    for (size_t index = 0; index < mapping->region_count; ++index) {
        int status = protect_region(
            &mapping->regions[index],
            0,
            mapping->regions[index].size,
            VM_PROT_NONE,
            mapping->page_size);
        if (status != BANGBANG_MACH_OK) {
            for (size_t prior = 0; prior < index; ++prior) {
                (void)protect_region(
                    &mapping->regions[prior],
                    0,
                    mapping->regions[prior].size,
                    VM_PROT_READ | VM_PROT_WRITE,
                    mapping->page_size);
            }
            return status;
        }
    }
    return BANGBANG_MACH_OK;
}

int bangbang_mach_lazy_mapping_restore_all_rw(
    struct bangbang_mach_lazy_mapping *mapping) {
    if (mapping == NULL) {
        return BANGBANG_MACH_INVALID;
    }
    int first_failure = BANGBANG_MACH_OK;
    for (size_t index = 0; index < mapping->region_count; ++index) {
        int status = protect_region(
            &mapping->regions[index],
            0,
            mapping->regions[index].size,
            VM_PROT_READ | VM_PROT_WRITE,
            mapping->page_size);
        if (first_failure == BANGBANG_MACH_OK &&
            status != BANGBANG_MACH_OK) {
            first_failure = status;
        }
    }
    return first_failure;
}

int bangbang_mach_lazy_mapping_allow(
    struct bangbang_mach_lazy_mapping *mapping,
    size_t region_index,
    size_t offset,
    size_t length,
    bool writable) {
    if (mapping == NULL || region_index >= mapping->region_count) {
        return BANGBANG_MACH_INVALID;
    }
    vm_prot_t protection = VM_PROT_READ;
    if (writable) {
        protection |= VM_PROT_WRITE;
    }
    return protect_region(
        &mapping->regions[region_index],
        offset,
        length,
        protection,
        mapping->page_size);
}

int bangbang_mach_lazy_mapping_hide(
    struct bangbang_mach_lazy_mapping *mapping,
    size_t region_index,
    size_t offset,
    size_t length) {
    if (mapping == NULL || region_index >= mapping->region_count) {
        return BANGBANG_MACH_INVALID;
    }
    return protect_region(
        &mapping->regions[region_index],
        offset,
        length,
        VM_PROT_NONE,
        mapping->page_size);
}

int bangbang_mach_lazy_mapping_zero_hidden(
    struct bangbang_mach_lazy_mapping *mapping,
    size_t region_index,
    size_t offset,
    size_t length) {
    if (mapping == NULL || region_index >= mapping->region_count) {
        return BANGBANG_MACH_INVALID;
    }
    struct bangbang_mach_region *region = &mapping->regions[region_index];
    if (offset > region->size || length == 0 ||
        length > region->size - offset ||
        offset % mapping->page_size != 0 ||
        length % mapping->page_size != 0) {
        return BANGBANG_MACH_INVALID;
    }
    uint8_t *destination = (uint8_t *)region->alias + offset;
    memset(destination, 0, length);
    atomic_thread_fence(memory_order_seq_cst);
    return BANGBANG_MACH_OK;
}

int bangbang_mach_lazy_mapping_publish(
    struct bangbang_mach_lazy_mapping *mapping,
    size_t region_index,
    size_t offset,
    const uint8_t *data,
    size_t length,
    bool zero,
    bool writable) {
    if (mapping == NULL || region_index >= mapping->region_count) {
        return BANGBANG_MACH_INVALID;
    }
    struct bangbang_mach_region *region = &mapping->regions[region_index];
    if (offset > region->size || length == 0 ||
        length > region->size - offset ||
        offset % mapping->page_size != 0 ||
        length % mapping->page_size != 0 ||
        (!zero && data == NULL)) {
        return BANGBANG_MACH_INVALID;
    }

    uint8_t *destination = (uint8_t *)region->alias + offset;
    if (zero) {
        memset(destination, 0, length);
    } else {
        memcpy(destination, data, length);
    }
    atomic_thread_fence(memory_order_seq_cst);

    return bangbang_mach_lazy_mapping_allow(
        mapping,
        region_index,
        offset,
        length,
        writable);
}

void bangbang_mach_lazy_mapping_destroy(
    struct bangbang_mach_lazy_mapping *mapping) {
    if (mapping == NULL) {
        return;
    }
    (void)bangbang_mach_lazy_mapping_restore_all_rw(mapping);
    for (size_t index = 0; index < mapping->region_count; ++index) {
        destroy_region_alias(&mapping->regions[index]);
    }
    free(mapping->regions);
    free(mapping);
}

static kern_return_t legacy_forward(
    mach_port_t port,
    exception_behavior_t behavior,
    thread_state_flavor_t flavor,
    mach_port_t thread,
    mach_port_t task,
    exception_type_t exception,
    mach_exception_data_t code,
    mach_msg_type_number_t code_count) {
    exception_data_type_t legacy_storage[2] = {0, 0};
    mach_msg_type_number_t legacy_count =
        code_count > 2 ? 2 : code_count;
    for (mach_msg_type_number_t index = 0; index < legacy_count; ++index) {
        legacy_storage[index] = (exception_data_type_t)code[index];
    }
    exception_data_t legacy_code = legacy_storage;
    exception_behavior_t base = behavior & ~MACH_EXCEPTION_MASK;

    if (base == EXCEPTION_DEFAULT) {
        return exception_raise(
            port,
            thread,
            task,
            exception,
            legacy_code,
            legacy_count);
    }

    thread_state_data_t old_state = {0};
    thread_state_data_t new_state = {0};
    mach_msg_type_number_t old_count = THREAD_STATE_MAX;
    kern_return_t result = thread_get_state(
        thread,
        flavor,
        old_state,
        &old_count);
    if (result != KERN_SUCCESS) {
        return result;
    }
    mach_msg_type_number_t new_count = THREAD_STATE_MAX;
    int forwarded_flavor = flavor;
    if (base == EXCEPTION_STATE) {
        result = exception_raise_state(
            port,
            exception,
            legacy_code,
            legacy_count,
            &forwarded_flavor,
            old_state,
            old_count,
            new_state,
            &new_count);
    } else if (base == EXCEPTION_STATE_IDENTITY) {
        result = exception_raise_state_identity(
            port,
            thread,
            task,
            exception,
            legacy_code,
            legacy_count,
            &forwarded_flavor,
            old_state,
            old_count,
            new_state,
            &new_count);
    } else {
        return KERN_FAILURE;
    }
    if (result != KERN_SUCCESS) {
        return result;
    }
    return thread_set_state(
        thread,
        forwarded_flavor,
        new_state,
        new_count);
}

static kern_return_t mach_forward(
    mach_port_t port,
    exception_behavior_t behavior,
    thread_state_flavor_t flavor,
    mach_port_t thread,
    mach_port_t task,
    exception_type_t exception,
    mach_exception_data_t code,
    mach_msg_type_number_t code_count) {
    exception_behavior_t base = behavior & ~MACH_EXCEPTION_MASK;
    if (base == EXCEPTION_DEFAULT) {
        return mach_exception_raise(
            port,
            thread,
            task,
            exception,
            code,
            code_count);
    }

    thread_state_data_t old_state = {0};
    thread_state_data_t new_state = {0};
    mach_msg_type_number_t old_count = THREAD_STATE_MAX;
    kern_return_t result = thread_get_state(
        thread,
        flavor,
        old_state,
        &old_count);
    if (result != KERN_SUCCESS) {
        return result;
    }
    mach_msg_type_number_t new_count = THREAD_STATE_MAX;
    int forwarded_flavor = flavor;
    if (base == EXCEPTION_STATE) {
        result = mach_exception_raise_state(
            port,
            exception,
            code,
            code_count,
            &forwarded_flavor,
            old_state,
            old_count,
            new_state,
            &new_count);
    } else if (base == EXCEPTION_STATE_IDENTITY) {
        result = mach_exception_raise_state_identity(
            port,
            thread,
            task,
            exception,
            code,
            code_count,
            &forwarded_flavor,
            old_state,
            old_count,
            new_state,
            &new_count);
    } else {
        return KERN_FAILURE;
    }
    if (result != KERN_SUCCESS) {
        return result;
    }
    return thread_set_state(
        thread,
        forwarded_flavor,
        new_state,
        new_count);
}

static kern_return_t forward_exception(
    const struct bangbang_mach_exception_owner *owner,
    mach_port_t thread,
    mach_port_t task,
    exception_type_t exception,
    mach_exception_data_t code,
    mach_msg_type_number_t code_count) {
    exception_mask_t mask = (exception_mask_t)(1U << exception);
    for (mach_msg_type_number_t index = 0;
         index < owner->previous_count;
         ++index) {
        if ((owner->previous_masks[index] & mask) == 0 ||
            owner->previous_ports[index] == MACH_PORT_NULL) {
            continue;
        }
        exception_behavior_t behavior = owner->previous_behaviors[index];
        if ((behavior & MACH_EXCEPTION_CODES) != 0) {
            return mach_forward(
                owner->previous_ports[index],
                behavior,
                owner->previous_flavors[index],
                thread,
                task,
                exception,
                code,
                code_count);
        }
        return legacy_forward(
            owner->previous_ports[index],
            behavior,
            owner->previous_flavors[index],
            thread,
            task,
            exception,
            code,
            code_count);
    }
    return KERN_FAILURE;
}

static bool classify_data_access(
    mach_port_t thread,
    mach_exception_data_t code,
    mach_msg_type_number_t code_count,
    uint64_t *address,
    uint32_t *access) {
    if (code_count < 2 || code[0] != KERN_PROTECTION_FAILURE ||
        code[1] <= 0) {
        return false;
    }
    arm_exception_state64_t state = {0};
    mach_msg_type_number_t state_count = ARM_EXCEPTION_STATE64_COUNT;
    kern_return_t result = thread_get_state(
        thread,
        ARM_EXCEPTION_STATE64,
        (thread_state_t)&state,
        &state_count);
    if (result != KERN_SUCCESS ||
        state_count != ARM_EXCEPTION_STATE64_COUNT ||
        state.__far != (uint64_t)code[1]) {
        return false;
    }
    uint32_t exception_class = state.__esr >> 26;
    if (exception_class != 0x24 && exception_class != 0x25) {
        return false;
    }
    *address = (uint64_t)code[1];
    *access = (state.__esr & (1U << 6)) == 0
        ? BANGBANG_MACH_ACCESS_READ
        : BANGBANG_MACH_ACCESS_WRITE;
    return true;
}

kern_return_t catch_mach_exception_raise(
    mach_port_t exception_port,
    mach_port_t thread,
    mach_port_t task,
    exception_type_t exception,
    mach_exception_data_t code,
    mach_msg_type_number_t code_count) {
    (void)exception_port;
    struct bangbang_mach_exception_owner *owner =
        atomic_load_explicit(&current_owner, memory_order_acquire);
    kern_return_t result = KERN_FAILURE;
    uint64_t address = 0;
    uint32_t access = 0;

    if (owner != NULL && exception == EXC_BAD_ACCESS &&
        classify_data_access(thread, code, code_count, &address, &access)) {
        uint32_t outcome = owner->callback(owner->context, address, access);
        if (outcome == BANGBANG_MACH_FAULT_HANDLED) {
            result = KERN_SUCCESS;
        } else if (outcome == BANGBANG_MACH_FAULT_TERMINAL) {
            _exit(BANGBANG_MACH_TERMINAL_EXIT_CODE);
        } else {
            result = forward_exception(
                owner,
                thread,
                task,
                exception,
                code,
                code_count);
        }
    } else if (owner != NULL) {
        result = forward_exception(
            owner,
            thread,
            task,
            exception,
            code,
            code_count);
    }

    if (thread != MACH_PORT_NULL) {
        (void)mach_port_deallocate(mach_task_self(), thread);
    }
    if (task != MACH_PORT_NULL) {
        (void)mach_port_deallocate(mach_task_self(), task);
    }
    return result;
}

kern_return_t catch_mach_exception_raise_state(
    mach_port_t exception_port,
    exception_type_t exception,
    const mach_exception_data_t code,
    mach_msg_type_number_t code_count,
    int *flavor,
    const thread_state_t old_state,
    mach_msg_type_number_t old_state_count,
    thread_state_t new_state,
    mach_msg_type_number_t *new_state_count) {
    (void)exception_port;
    (void)exception;
    (void)code;
    (void)code_count;
    (void)flavor;
    (void)old_state;
    (void)old_state_count;
    (void)new_state;
    (void)new_state_count;
    return KERN_FAILURE;
}

kern_return_t catch_mach_exception_raise_state_identity(
    mach_port_t exception_port,
    mach_port_t thread,
    mach_port_t task,
    exception_type_t exception,
    mach_exception_data_t code,
    mach_msg_type_number_t code_count,
    int *flavor,
    thread_state_t old_state,
    mach_msg_type_number_t old_state_count,
    thread_state_t new_state,
    mach_msg_type_number_t *new_state_count) {
    (void)exception_port;
    (void)exception;
    (void)code;
    (void)code_count;
    (void)flavor;
    (void)old_state;
    (void)old_state_count;
    (void)new_state;
    (void)new_state_count;
    if (thread != MACH_PORT_NULL) {
        (void)mach_port_deallocate(mach_task_self(), thread);
    }
    if (task != MACH_PORT_NULL) {
        (void)mach_port_deallocate(mach_task_self(), task);
    }
    return KERN_FAILURE;
}

static void *exception_server_main(void *context) {
    struct bangbang_mach_exception_owner *owner = context;
    while (!atomic_load_explicit(&owner->stop, memory_order_acquire)) {
        kern_return_t result = mach_msg_server_once(
            mach_exc_server,
            BANGBANG_MACH_SERVER_MESSAGE_SIZE,
            owner->port,
            MACH_MSG_OPTION_NONE);
        if (atomic_load_explicit(&owner->stop, memory_order_acquire)) {
            break;
        }
        if (result != KERN_SUCCESS) {
            _exit(BANGBANG_MACH_TERMINAL_EXIT_CODE);
        }
    }
    return NULL;
}

static void release_previous_ports(
    struct bangbang_mach_exception_owner *owner) {
    for (mach_msg_type_number_t index = 0;
         index < owner->previous_count;
         ++index) {
        if (owner->previous_ports[index] != MACH_PORT_NULL) {
            (void)mach_port_deallocate(
                mach_task_self(),
                owner->previous_ports[index]);
            owner->previous_ports[index] = MACH_PORT_NULL;
        }
    }
    owner->previous_count = 0;
}

static void stop_exception_thread(
    struct bangbang_mach_exception_owner *owner) {
    if (!owner->thread_started) {
        return;
    }
    atomic_store_explicit(&owner->stop, true, memory_order_release);
    mach_msg_header_t message = {0};
    message.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    message.msgh_size = (mach_msg_size_t)sizeof(message);
    message.msgh_remote_port = owner->port;
    message.msgh_local_port = MACH_PORT_NULL;
    message.msgh_id = BANGBANG_MACH_STOP_MESSAGE_ID;
    kern_return_t result = mach_msg(
        &message,
        MACH_SEND_MSG,
        message.msgh_size,
        0,
        MACH_PORT_NULL,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL);
    if (result != KERN_SUCCESS) {
        (void)mach_port_mod_refs(
            mach_task_self(),
            owner->port,
            MACH_PORT_RIGHT_RECEIVE,
            -1);
    }
    (void)pthread_join(owner->thread, NULL);
    owner->thread_started = false;
}

static void destroy_exception_port(
    struct bangbang_mach_exception_owner *owner) {
    if (owner->port == MACH_PORT_NULL) {
        return;
    }
    mach_port_type_t type = 0;
    if (mach_port_type(mach_task_self(), owner->port, &type) == KERN_SUCCESS &&
        (type & MACH_PORT_TYPE_RECEIVE) != 0) {
        (void)mach_port_mod_refs(
            mach_task_self(),
            owner->port,
            MACH_PORT_RIGHT_RECEIVE,
            -1);
    }
    if (mach_port_type(mach_task_self(), owner->port, &type) == KERN_SUCCESS &&
        (type & (MACH_PORT_TYPE_SEND | MACH_PORT_TYPE_DEAD_NAME)) != 0) {
        (void)mach_port_deallocate(mach_task_self(), owner->port);
    }
    owner->port = MACH_PORT_NULL;
}

static void cleanup_uninstalled_owner(
    struct bangbang_mach_exception_owner *owner) {
    stop_exception_thread(owner);
    destroy_exception_port(owner);
    release_previous_ports(owner);
    struct bangbang_mach_exception_owner *expected = owner;
    (void)atomic_compare_exchange_strong_explicit(
        &current_owner,
        &expected,
        NULL,
        memory_order_acq_rel,
        memory_order_acquire);
    free(owner);
}

int bangbang_mach_exception_owner_install(
    void *context,
    bangbang_mach_fault_callback callback,
    struct bangbang_mach_exception_owner **output) {
    if (context == NULL || callback == NULL || output == NULL) {
        return BANGBANG_MACH_INVALID;
    }
    *output = NULL;
    if (pthread_mutex_lock(&owner_install_lock) != 0) {
        return BANGBANG_MACH_THREAD_FAILED;
    }
    if (atomic_load_explicit(&current_owner, memory_order_acquire) != NULL) {
        (void)pthread_mutex_unlock(&owner_install_lock);
        return BANGBANG_MACH_OWNER_BUSY;
    }

    struct bangbang_mach_exception_owner *owner =
        calloc(1, sizeof(*owner));
    if (owner == NULL) {
        (void)pthread_mutex_unlock(&owner_install_lock);
        return BANGBANG_MACH_NO_MEMORY;
    }
    owner->context = context;
    owner->callback = callback;
    atomic_init(&owner->stop, false);

    kern_return_t result = mach_port_allocate(
        mach_task_self(),
        MACH_PORT_RIGHT_RECEIVE,
        &owner->port);
    if (result == KERN_SUCCESS) {
        result = mach_port_insert_right(
            mach_task_self(),
            owner->port,
            owner->port,
            MACH_MSG_TYPE_MAKE_SEND);
    }
    if (result != KERN_SUCCESS) {
        cleanup_uninstalled_owner(owner);
        (void)pthread_mutex_unlock(&owner_install_lock);
        return BANGBANG_MACH_OPERATION_FAILED;
    }

    atomic_store_explicit(&current_owner, owner, memory_order_release);
    if (pthread_create(
            &owner->thread,
            NULL,
            exception_server_main,
            owner) != 0) {
        cleanup_uninstalled_owner(owner);
        (void)pthread_mutex_unlock(&owner_install_lock);
        return BANGBANG_MACH_THREAD_FAILED;
    }
    owner->thread_started = true;

    owner->previous_count = EXC_TYPES_COUNT;
    result = task_swap_exception_ports(
        mach_task_self(),
        EXC_MASK_BAD_ACCESS,
        owner->port,
        EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES,
        THREAD_STATE_NONE,
        owner->previous_masks,
        &owner->previous_count,
        owner->previous_ports,
        owner->previous_behaviors,
        owner->previous_flavors);
    if (result != KERN_SUCCESS) {
        cleanup_uninstalled_owner(owner);
        (void)pthread_mutex_unlock(&owner_install_lock);
        return BANGBANG_MACH_OPERATION_FAILED;
    }
    for (mach_msg_type_number_t index = 0;
         index < owner->previous_count;
         ++index) {
        owner->previous_masks[index] &= EXC_MASK_BAD_ACCESS;
    }
    owner->installed = true;
    *output = owner;
    (void)pthread_mutex_unlock(&owner_install_lock);
    return BANGBANG_MACH_OK;
}

static void release_handler_query_ports(
    mach_port_t *ports,
    mach_msg_type_number_t count) {
    for (mach_msg_type_number_t index = 0; index < count; ++index) {
        if (ports[index] != MACH_PORT_NULL) {
            (void)mach_port_deallocate(mach_task_self(), ports[index]);
        }
    }
}

static int restore_previous_if_current(
    struct bangbang_mach_exception_owner *owner,
    bool *restored) {
    exception_mask_t masks[EXC_TYPES_COUNT] = {0};
    mach_port_t ports[EXC_TYPES_COUNT] = {0};
    exception_behavior_t behaviors[EXC_TYPES_COUNT] = {0};
    thread_state_flavor_t flavors[EXC_TYPES_COUNT] = {0};
    mach_msg_type_number_t count = EXC_TYPES_COUNT;
    kern_return_t result = task_get_exception_ports(
        mach_task_self(),
        EXC_MASK_BAD_ACCESS,
        masks,
        &count,
        ports,
        behaviors,
        flavors);
    if (result != KERN_SUCCESS) {
        return BANGBANG_MACH_RESTORE_FAILED;
    }

    bool owns_slot = false;
    for (mach_msg_type_number_t index = 0; index < count; ++index) {
        if ((masks[index] & EXC_MASK_BAD_ACCESS) != 0 &&
            ports[index] == owner->port &&
            behaviors[index] ==
                (exception_behavior_t)(
                    EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES) &&
            flavors[index] == THREAD_STATE_NONE) {
            owns_slot = true;
            break;
        }
    }
    release_handler_query_ports(ports, count);
    if (!owns_slot) {
        *restored = false;
        return BANGBANG_MACH_OK;
    }

    result = KERN_SUCCESS;
    bool restored_any = false;
    if (owner->previous_count != 0) {
        for (mach_msg_type_number_t index = 0;
             index < owner->previous_count;
             ++index) {
            if (owner->previous_masks[index] == 0) {
                continue;
            }
            kern_return_t one = task_set_exception_ports(
                mach_task_self(),
                owner->previous_masks[index],
                owner->previous_ports[index],
                owner->previous_behaviors[index],
                owner->previous_flavors[index]);
            if (result == KERN_SUCCESS && one != KERN_SUCCESS) {
                result = one;
            }
            restored_any = true;
        }
    }
    if (!restored_any) {
        result = task_set_exception_ports(
            mach_task_self(),
            EXC_MASK_BAD_ACCESS,
            MACH_PORT_NULL,
            EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES,
            THREAD_STATE_NONE);
    }
    if (result != KERN_SUCCESS) {
        return BANGBANG_MACH_RESTORE_FAILED;
    }
    *restored = true;
    return BANGBANG_MACH_OK;
}

int bangbang_mach_exception_owner_shutdown(
    struct bangbang_mach_exception_owner *owner,
    bool *restored) {
    if (owner == NULL || restored == NULL || !owner->installed) {
        return BANGBANG_MACH_INVALID;
    }
    *restored = false;
    if (pthread_mutex_lock(&owner_install_lock) != 0) {
        return BANGBANG_MACH_THREAD_FAILED;
    }
    int status = restore_previous_if_current(owner, restored);
    if (status != BANGBANG_MACH_OK) {
        (void)pthread_mutex_unlock(&owner_install_lock);
        return status;
    }

    owner->installed = false;
    stop_exception_thread(owner);
    struct bangbang_mach_exception_owner *expected = owner;
    (void)atomic_compare_exchange_strong_explicit(
        &current_owner,
        &expected,
        NULL,
        memory_order_acq_rel,
        memory_order_acquire);
    destroy_exception_port(owner);
    release_previous_ports(owner);
    free(owner);
    (void)pthread_mutex_unlock(&owner_install_lock);
    return BANGBANG_MACH_OK;
}
