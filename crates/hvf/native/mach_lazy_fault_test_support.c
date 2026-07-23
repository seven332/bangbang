#include "bangbang_mach_exc_server.h"

#include <mach/exception_types.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>

#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum {
    BANGBANG_MACH_TEST_OK = 0,
    BANGBANG_MACH_TEST_INVALID = 1,
    BANGBANG_MACH_TEST_OPERATION_FAILED = 3,
    BANGBANG_MACH_TEST_THREAD_FAILED = 6,
    BANGBANG_MACH_TEST_SERVER_MESSAGE_SIZE = 32768,
    BANGBANG_MACH_TEST_STOP_MESSAGE_ID = 0x42425446,
    BANGBANG_MACH_EXCEPTION_RAISE_ID = 2405,
};

struct bangbang_mach_test_handler {
    mach_port_t port;
    pthread_t thread;
    bool thread_started;
    bool installed;
    _Atomic bool stop;
    void *target;
    size_t target_size;
    _Atomic size_t handled_count;
    exception_mask_t previous_masks[EXC_TYPES_COUNT];
    mach_port_t previous_ports[EXC_TYPES_COUNT];
    exception_behavior_t previous_behaviors[EXC_TYPES_COUNT];
    thread_state_flavor_t previous_flavors[EXC_TYPES_COUNT];
    mach_msg_type_number_t previous_count;
};

int bangbang_mach_test_handler_install(
    void *target,
    size_t target_size,
    struct bangbang_mach_test_handler **output);
int bangbang_mach_test_handler_reinstall(
    struct bangbang_mach_test_handler *handler);
int bangbang_mach_test_handler_is_current(
    struct bangbang_mach_test_handler *handler,
    bool *current);
size_t bangbang_mach_test_handler_handled_count(
    const struct bangbang_mach_test_handler *handler);
int bangbang_mach_test_handler_shutdown(
    struct bangbang_mach_test_handler *handler,
    bool *restored);

static _Atomic(struct bangbang_mach_test_handler *) current_test_handler;
static pthread_mutex_t test_handler_lock = PTHREAD_MUTEX_INITIALIZER;

static bool checked_target_range(
    const struct bangbang_mach_test_handler *handler,
    uint64_t address) {
    uintptr_t start = (uintptr_t)handler->target;
    return start != 0 && handler->target_size != 0 &&
        start <= UINTPTR_MAX - handler->target_size &&
        address >= (uint64_t)start &&
        address < (uint64_t)(start + handler->target_size);
}

static void release_message_port(mach_port_t port) {
    if (port != MACH_PORT_NULL) {
        (void)mach_port_deallocate(mach_task_self(), port);
    }
}

static boolean_t test_handler_demux(
    mach_msg_header_t *input,
    mach_msg_header_t *output) {
    mig_reply_error_t *reply = (mig_reply_error_t *)output;
    reply->Head.msgh_bits =
        MACH_MSGH_BITS(MACH_MSGH_BITS_REMOTE(input->msgh_bits), 0);
    reply->Head.msgh_remote_port = input->msgh_remote_port;
    reply->Head.msgh_size = (mach_msg_size_t)sizeof(*reply);
    reply->Head.msgh_local_port = MACH_PORT_NULL;
    reply->Head.msgh_id = input->msgh_id + 100;
    reply->Head.msgh_reserved = 0;
    reply->NDR = NDR_record;
    reply->RetCode = MIG_BAD_ID;

    if (input->msgh_id != BANGBANG_MACH_EXCEPTION_RAISE_ID ||
        input->msgh_size < sizeof(__Request__mach_exception_raise_t) ||
        (input->msgh_bits & MACH_MSGH_BITS_COMPLEX) == 0) {
        return FALSE;
    }

    __Request__mach_exception_raise_t *request =
        (__Request__mach_exception_raise_t *)input;
    if (request->msgh_body.msgh_descriptor_count != 2 ||
        request->thread.type != MACH_MSG_PORT_DESCRIPTOR ||
        request->task.type != MACH_MSG_PORT_DESCRIPTOR) {
        reply->RetCode = MIG_BAD_ARGUMENTS;
        return TRUE;
    }

    struct bangbang_mach_test_handler *handler =
        atomic_load_explicit(&current_test_handler, memory_order_acquire);
    kern_return_t result = KERN_FAILURE;
    if (handler != NULL && request->exception == EXC_BAD_ACCESS &&
        request->codeCnt >= 2 &&
        request->code[0] == KERN_PROTECTION_FAILURE &&
        request->code[1] > 0 &&
        checked_target_range(handler, (uint64_t)request->code[1])) {
        result = mach_vm_protect(
            mach_task_self(),
            (mach_vm_address_t)(uintptr_t)handler->target,
            (mach_vm_size_t)handler->target_size,
            FALSE,
            VM_PROT_READ | VM_PROT_WRITE);
        if (result == KERN_SUCCESS) {
            atomic_fetch_add_explicit(
                &handler->handled_count,
                1,
                memory_order_relaxed);
        }
    }

    release_message_port(request->thread.name);
    release_message_port(request->task.name);
    reply->RetCode = result;
    return TRUE;
}

static void *test_handler_server_main(void *context) {
    struct bangbang_mach_test_handler *handler = context;
    while (!atomic_load_explicit(&handler->stop, memory_order_acquire)) {
        kern_return_t result = mach_msg_server_once(
            test_handler_demux,
            BANGBANG_MACH_TEST_SERVER_MESSAGE_SIZE,
            handler->port,
            MACH_MSG_OPTION_NONE);
        if (atomic_load_explicit(&handler->stop, memory_order_acquire)) {
            break;
        }
        if (result != KERN_SUCCESS) {
            break;
        }
    }
    return NULL;
}

static void release_previous_ports(
    struct bangbang_mach_test_handler *handler) {
    for (mach_msg_type_number_t index = 0;
         index < handler->previous_count;
         ++index) {
        release_message_port(handler->previous_ports[index]);
        handler->previous_ports[index] = MACH_PORT_NULL;
    }
    handler->previous_count = 0;
}

static void stop_test_handler_thread(
    struct bangbang_mach_test_handler *handler) {
    if (!handler->thread_started) {
        return;
    }
    atomic_store_explicit(&handler->stop, true, memory_order_release);
    mach_msg_header_t message = {0};
    message.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    message.msgh_size = (mach_msg_size_t)sizeof(message);
    message.msgh_remote_port = handler->port;
    message.msgh_local_port = MACH_PORT_NULL;
    message.msgh_id = BANGBANG_MACH_TEST_STOP_MESSAGE_ID;
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
            handler->port,
            MACH_PORT_RIGHT_RECEIVE,
            -1);
    }
    (void)pthread_join(handler->thread, NULL);
    handler->thread_started = false;
}

static void destroy_test_handler_port(
    struct bangbang_mach_test_handler *handler) {
    if (handler->port == MACH_PORT_NULL) {
        return;
    }
    mach_port_type_t type = 0;
    if (mach_port_type(mach_task_self(), handler->port, &type) ==
            KERN_SUCCESS &&
        (type & MACH_PORT_TYPE_RECEIVE) != 0) {
        (void)mach_port_mod_refs(
            mach_task_self(),
            handler->port,
            MACH_PORT_RIGHT_RECEIVE,
            -1);
    }
    if (mach_port_type(mach_task_self(), handler->port, &type) ==
            KERN_SUCCESS &&
        (type & (MACH_PORT_TYPE_SEND | MACH_PORT_TYPE_DEAD_NAME)) != 0) {
        release_message_port(handler->port);
    }
    handler->port = MACH_PORT_NULL;
}

static void cleanup_test_handler(
    struct bangbang_mach_test_handler *handler) {
    stop_test_handler_thread(handler);
    destroy_test_handler_port(handler);
    release_previous_ports(handler);
    struct bangbang_mach_test_handler *expected = handler;
    (void)atomic_compare_exchange_strong_explicit(
        &current_test_handler,
        &expected,
        NULL,
        memory_order_acq_rel,
        memory_order_acquire);
    free(handler);
}

static void release_query_ports(
    mach_port_t *ports,
    mach_msg_type_number_t count) {
    for (mach_msg_type_number_t index = 0; index < count; ++index) {
        release_message_port(ports[index]);
    }
}

static int query_handler_current(
    struct bangbang_mach_test_handler *handler,
    bool *current) {
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
        return BANGBANG_MACH_TEST_OPERATION_FAILED;
    }

    *current = false;
    for (mach_msg_type_number_t index = 0; index < count; ++index) {
        if ((masks[index] & EXC_MASK_BAD_ACCESS) != 0 &&
            ports[index] == handler->port &&
            behaviors[index] ==
                (exception_behavior_t)(
                    EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES) &&
            flavors[index] == THREAD_STATE_NONE) {
            *current = true;
            break;
        }
    }
    release_query_ports(ports, count);
    return BANGBANG_MACH_TEST_OK;
}

static int restore_previous_if_current(
    struct bangbang_mach_test_handler *handler,
    bool *restored) {
    bool current = false;
    int status = query_handler_current(handler, &current);
    if (status != BANGBANG_MACH_TEST_OK || !current) {
        *restored = false;
        return status;
    }

    kern_return_t result = KERN_SUCCESS;
    bool restored_any = false;
    if (handler->previous_count != 0) {
        for (mach_msg_type_number_t index = 0;
             index < handler->previous_count;
             ++index) {
            if (handler->previous_masks[index] == 0) {
                continue;
            }
            kern_return_t one = task_set_exception_ports(
                mach_task_self(),
                handler->previous_masks[index],
                handler->previous_ports[index],
                handler->previous_behaviors[index],
                handler->previous_flavors[index]);
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
        return BANGBANG_MACH_TEST_OPERATION_FAILED;
    }
    *restored = true;
    return BANGBANG_MACH_TEST_OK;
}

int bangbang_mach_test_handler_install(
    void *target,
    size_t target_size,
    struct bangbang_mach_test_handler **output) {
    if (target == NULL || target_size == 0 || output == NULL) {
        return BANGBANG_MACH_TEST_INVALID;
    }
    *output = NULL;
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0 || (uintptr_t)target % (uintptr_t)page_size != 0 ||
        target_size % (size_t)page_size != 0) {
        return BANGBANG_MACH_TEST_INVALID;
    }
    if (pthread_mutex_lock(&test_handler_lock) != 0) {
        return BANGBANG_MACH_TEST_THREAD_FAILED;
    }
    if (atomic_load_explicit(
            &current_test_handler,
            memory_order_acquire) != NULL) {
        (void)pthread_mutex_unlock(&test_handler_lock);
        return BANGBANG_MACH_TEST_OPERATION_FAILED;
    }

    struct bangbang_mach_test_handler *handler =
        calloc(1, sizeof(*handler));
    if (handler == NULL) {
        (void)pthread_mutex_unlock(&test_handler_lock);
        return BANGBANG_MACH_TEST_OPERATION_FAILED;
    }
    handler->target = target;
    handler->target_size = target_size;
    atomic_init(&handler->stop, false);
    atomic_init(&handler->handled_count, 0);

    kern_return_t result = mach_port_allocate(
        mach_task_self(),
        MACH_PORT_RIGHT_RECEIVE,
        &handler->port);
    if (result == KERN_SUCCESS) {
        result = mach_port_insert_right(
            mach_task_self(),
            handler->port,
            handler->port,
            MACH_MSG_TYPE_MAKE_SEND);
    }
    if (result != KERN_SUCCESS) {
        cleanup_test_handler(handler);
        (void)pthread_mutex_unlock(&test_handler_lock);
        return BANGBANG_MACH_TEST_OPERATION_FAILED;
    }

    atomic_store_explicit(
        &current_test_handler,
        handler,
        memory_order_release);
    if (pthread_create(
            &handler->thread,
            NULL,
            test_handler_server_main,
            handler) != 0) {
        cleanup_test_handler(handler);
        (void)pthread_mutex_unlock(&test_handler_lock);
        return BANGBANG_MACH_TEST_THREAD_FAILED;
    }
    handler->thread_started = true;

    handler->previous_count = EXC_TYPES_COUNT;
    result = task_swap_exception_ports(
        mach_task_self(),
        EXC_MASK_BAD_ACCESS,
        handler->port,
        EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES,
        THREAD_STATE_NONE,
        handler->previous_masks,
        &handler->previous_count,
        handler->previous_ports,
        handler->previous_behaviors,
        handler->previous_flavors);
    if (result != KERN_SUCCESS) {
        cleanup_test_handler(handler);
        (void)pthread_mutex_unlock(&test_handler_lock);
        return BANGBANG_MACH_TEST_OPERATION_FAILED;
    }
    for (mach_msg_type_number_t index = 0;
         index < handler->previous_count;
         ++index) {
        handler->previous_masks[index] &= EXC_MASK_BAD_ACCESS;
    }
    handler->installed = true;
    *output = handler;
    (void)pthread_mutex_unlock(&test_handler_lock);
    return BANGBANG_MACH_TEST_OK;
}

int bangbang_mach_test_handler_reinstall(
    struct bangbang_mach_test_handler *handler) {
    if (handler == NULL || !handler->installed) {
        return BANGBANG_MACH_TEST_INVALID;
    }
    kern_return_t result = task_set_exception_ports(
        mach_task_self(),
        EXC_MASK_BAD_ACCESS,
        handler->port,
        EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES,
        THREAD_STATE_NONE);
    return result == KERN_SUCCESS
        ? BANGBANG_MACH_TEST_OK
        : BANGBANG_MACH_TEST_OPERATION_FAILED;
}

int bangbang_mach_test_handler_is_current(
    struct bangbang_mach_test_handler *handler,
    bool *current) {
    if (handler == NULL || current == NULL || !handler->installed) {
        return BANGBANG_MACH_TEST_INVALID;
    }
    return query_handler_current(handler, current);
}

size_t bangbang_mach_test_handler_handled_count(
    const struct bangbang_mach_test_handler *handler) {
    if (handler == NULL) {
        return 0;
    }
    return atomic_load_explicit(
        &handler->handled_count,
        memory_order_relaxed);
}

int bangbang_mach_test_handler_shutdown(
    struct bangbang_mach_test_handler *handler,
    bool *restored) {
    if (handler == NULL || restored == NULL || !handler->installed) {
        return BANGBANG_MACH_TEST_INVALID;
    }
    *restored = false;
    if (pthread_mutex_lock(&test_handler_lock) != 0) {
        return BANGBANG_MACH_TEST_THREAD_FAILED;
    }
    int status = restore_previous_if_current(handler, restored);
    if (status != BANGBANG_MACH_TEST_OK) {
        (void)pthread_mutex_unlock(&test_handler_lock);
        return status;
    }
    handler->installed = false;
    cleanup_test_handler(handler);
    (void)pthread_mutex_unlock(&test_handler_lock);
    return BANGBANG_MACH_TEST_OK;
}
