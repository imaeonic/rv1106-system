
ifeq ($(APP_PARAM), )
    APP_PARAM:=../Makefile.param
    include $(APP_PARAM)
endif

export LC_ALL=C
SHELL:=/bin/bash

CURRENT_DIR := $(shell pwd)

PKG_NAME := jetkvm
PKG_BIN ?= out
PKG_BUILD ?= build
JETKVM_RDP_BIN ?= $(CURRENT_DIR)/rdp-console/target/armv7-unknown-linux-musleabihf/release/jetkvm-rdp

ifeq ($(PKG_BIN),)
$(error ### $(CURRENT_DIR): PKG_BIN is NULL, Please Check !!!)
endif

ifeq ($(RK_APP_TYPE),JETKVM)
    PKG_TARGET := jetkvm-build
endif

all: $(PKG_TARGET) thirdlibs-build
	@echo "build $(PKG_NAME) done"

thirdlibs-build:
	@test ! -d $(RK_APP_COMPONENT)/third_libs || make -C $(RK_APP_COMPONENT)/third_libs

jetkvm-build: 
	mkdir -p $(PKG_BIN)/bin $(PKG_BIN)/share $(PKG_BIN)/install_to_userdata
	cp $(CURRENT_DIR)/jetkvm-lib.sh $(PKG_BIN)/share/jetkvm-lib.sh
	cp $(CURRENT_DIR)/RkLunch.sh $(PKG_BIN)/bin/RkLunch.sh
	cp $(CURRENT_DIR)/RkLunch-stop.sh $(PKG_BIN)/bin/RkLunch-stop.sh
	cp $(CURRENT_DIR)/dropbear.sh $(PKG_BIN)/bin/dropbear.sh
	cp $(CURRENT_DIR)/fb_init $(PKG_BIN)/share/fb_init
	cp -rfa $(CURRENT_DIR)/jetkvm $(PKG_BIN)/install_to_userdata
	@if [ -f "$(JETKVM_RDP_BIN)" ]; then \
		echo "[INSTALL] JetKVM RDP daemon: $(JETKVM_RDP_BIN)"; \
		install -m 0755 "$(JETKVM_RDP_BIN)" $(PKG_BIN)/install_to_userdata/jetkvm/bin/jetkvm-rdp; \
	else \
		echo "[INFO] JetKVM RDP daemon not supplied; firmware will retain normal web-only behaviour"; \
	fi
	$(call MAROC_COPY_PKG_TO_APP_OUTPUT, $(RK_APP_OUTPUT), $(PKG_BIN))

clean:
	@rm -rf $(PKG_BIN) $(PKG_BUILD)

distclean: clean